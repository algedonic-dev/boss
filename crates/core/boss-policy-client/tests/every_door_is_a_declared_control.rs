//! Every door asks policy through a declared control — the pin on the
//! collapse of design 1c4e42e1 decision 8 (backlog 47aed706).
//!
//! WHAT THE COMPILER NOW HOLDS. A door asks for a static pair through a
//! `controls::` const (`PolicyClient::ask`, `scope_of`, and the
//! `writes::` helpers all take one), and the `controls!` line declaring
//! the const is the line that lists it in `CONTROLS`, which coverage
//! reads. So the static control list cannot miss a door that asks
//! through a const — car 1's hand-kept `DOORS` table, one count and one
//! digest per door file, is gone (CLAUDE.md §9a: collapse when you can).
//!
//! WHAT IS LEFT FOR A SCAN. The compiler cannot stop a door asking with
//! a raw `Action::<verb>` — the port still takes one, because some
//! resources are data (`step-signoff:<role>`, `job:<kind>`, a View's
//! source) and the policy service judges a rule write by the write's
//! own verb. So the scan car 1 wrote still runs over every crate's
//! source, and a file it sees asking raw must be one of [`RAW_ASKS`],
//! each with the reason its resource is data, held to its count and to
//! the digest of its ask lines (car 1's review, L1). A door that asks
//! for a static pair raw is refused, naming the const to ask instead.
//! Since the release review of car 2 (005eb5b4, LOW-1) "raw" also means
//! the four shapes that hid an ask from the first scan: a check naming
//! its resource with no `Action::` in sight (a const's `.action()` with
//! another resource), a glob or braced import of the verbs, a verb parsed
//! from text, and any ask below a test-only item in the middle of a file
//! (the cut is the test module now, not the first `#[cfg(test)]`).
//! Two more pins ride beside it: every declared control is asked by
//! some door (a const no door asks would be a control coverage reports
//! that nothing checks), and every `scope_of` read names a `READ_`
//! control (the predicate is always a read).
//!
//! tree-wide pin — it scans every crate's source under crates/, so a door
//! added in any other crate changes its verdict.

use std::collections::BTreeMap;
use std::path::Path;

use boss_policy_client::controls::CONTROLS;

/// The files that still ask policy with a raw `Action`, and why each
/// resource is data rather than a declared control. `mentions` is how
/// many raw asks the scan sees; `digest` hashes those lines and the
/// resource tokens beside them, so an ask changed in place fails too.
const RAW_ASKS: &[RawAsk] = &[
    RawAsk {
        file: "crates/core/boss-jobs/src/http/signer.rs",
        mentions: 1,
        digest: "db53570313edd2cf",
        why: "signer SignOff on step-signoff:<role>, a required role named by the admitted workflow; static step Update uses UPDATE_STEP",
    },
    RawAsk {
        file: "crates/core/boss-jobs/src/http/steps.rs",
        mentions: 2,
        digest: "3abc76e81177aaa0",
        why: "sign-off on step-signoff:<role>, for a role only a workflow names — coverage \
              reads it off every active workflow",
    },
    RawAsk {
        file: "crates/core/boss-jobs/src/open_authority.rs",
        mentions: 1,
        digest: "b1fdad01b5d1867d",
        why: "create on job:<kind>, the per-kind grant beside CREATE_JOB (design 222fc982) — \
              a holder of CREATE_JOB holds every kind",
    },
    RawAsk {
        file: "crates/core/boss-policy/src/authority.rs",
        mentions: 11,
        digest: "a1927bd3b3652996",
        why: "the policy service's judge of a rule write: the verb is the write's own \
              (create, update or delete), and the authority it reads was asked through \
              POLICY_WRITES",
    },
    RawAsk {
        file: "crates/core/boss-views/src/query.rs",
        mentions: 1,
        digest: "3c00822ae37dab92",
        why: "a View names its own source resource — data, unmeasured",
    },
    RawAsk {
        file: "crates/core/boss-policy-client/src/engine.rs",
        mentions: 1,
        digest: "0107313527a6731e",
        why: "the engine's scope_predicate is check(Read) on the resource its caller names",
    },
    RawAsk {
        file: "crates/core/boss-policy-client/src/coverage.rs",
        mentions: 1,
        digest: "84462133fa9e6b4e",
        why: "coverage itself, not a door: it imports SignOff and Update bare to build the \
              step-signoff controls off the workflows and to read the claim door's three \
              routes (seen since review 005eb5b4 made a braced import count)",
    },
    RawAsk {
        file: "crates/core/boss-policy-client/src/defaults.rs",
        mentions: 1,
        digest: "b4dd46a563156089",
        why: "the shipped default rules, a grant table and not a door: it imports every verb \
              bare to spell the platform roles' rows (seen since review 005eb5b4 made a glob \
              import count)",
    },
    RawAsk {
        file: "crates/core/boss-policy-client/src/lib.rs",
        mentions: 4,
        digest: "a428b54a5135ea96",
        why: "the port and its adapters: a scope read is check(Read) on the caller's resource",
    },
    RawAsk {
        file: "crates/core/boss-policy-client/src/role_reporting.rs",
        mentions: 6,
        digest: "0698f2b27d97eae4",
        why: "the report-only PolicyClient decorator forwards the caller's dynamic action/resource; \
              a scope read compares Read on that same caller-supplied resource",
    },
];

struct RawAsk {
    file: &'static str,
    mentions: usize,
    digest: &'static str,
    why: &'static str,
}

const VERBS: [&str; 8] = [
    "Read", "Create", "Update", "Close", "SignOff", "Delete", "Publish", "Retire",
];

/// How far from an ask line a resource token is read into the digest.
const RESOURCE_WINDOW: usize = 3;

/// An identifier byte. Every needle is ASCII, so byte offsets are char
/// boundaries around it; a non-ASCII neighbour counts as identifier,
/// which can only make the scan stricter.
fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || !b.is_ascii()
}

/// The forms that hand a door a verb no `Action::<verb>` names: a glob or
/// braced import (every bare `Create` after it is invisible to the
/// scan), and a verb parsed from text (review 005eb5b4 of car 2, LOW-1).
const VERB_SOURCES: [&str; 4] = [
    "Action::*",
    "Action::{",
    "Action::from_str(",
    "parse::<Action>",
];

/// The calls that ask policy a pair: a check whose resource is named on
/// the call itself is an ask, whatever names its verb.
const CHECK_CALLS: [&str; 2] = [".check(", ".check_until("];

/// How many raw policy asks the scan sees on one line of code: an
/// `Action::<verb>` standing alone, a `.scope_predicate(` call, or a verb
/// brought in bare or parsed ([`VERB_SOURCES`]).
fn asks_on(line: &str) -> usize {
    let bytes = line.as_bytes();
    let alone_before = |at: usize| at == 0 || !ident(bytes[at - 1]);
    let alone_after = |at: usize| at >= bytes.len() || !ident(bytes[at]);
    let verbs = line
        .match_indices("Action::")
        .filter(|(at, _)| alone_before(*at))
        .filter(|(at, needle)| {
            let rest = &line[at + needle.len()..];
            VERBS
                .iter()
                .any(|v| rest.starts_with(v) && alone_after(at + needle.len() + v.len()))
        })
        .count();
    let sources: usize = VERB_SOURCES
        .iter()
        .map(|needle| {
            line.match_indices(needle)
                .filter(|(at, _)| alone_before(*at))
                .count()
        })
        .sum();
    verbs + sources + line.matches(".scope_predicate(").count()
}

/// Checks whose verb the scan cannot see but whose resource it can: a
/// `.check(` or `.check_until(` call with a `Resource::<name>` on its
/// line or the [`RESOURCE_WINDOW`] lines after it, and no counted ask in
/// that span — `check(user, CREATE_CLASS.action(), Resource::ledger())`
/// asks a static pair raw with no `Action::` in sight (005eb5b4, LOW-1).
/// A check whose resource is data (`body.resource`) names none.
fn unnamed_verb_checks(lines: &[&str]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| CHECK_CALLS.iter().any(|c| l.contains(c)))
        .filter_map(|(i, _)| {
            let to = (i + RESOURCE_WINDOW).min(lines.len().saturating_sub(1));
            let span = &lines[i..=to];
            let named = span.iter().any(|l| !resource_tokens(l).is_empty());
            let counted = span.iter().any(|l| asks_on(l) > 0);
            (named && !counted).then_some(i)
        })
        .collect()
}

/// The code lines of one source file the scan reads: everything before
/// its test module — a `#[cfg(test)]` whose item is a `mod` — with a
/// comment line blanked (kept, so the resource window counts real
/// lines). Until review 005eb5b4 (LOW-1) the cut was the FIRST
/// `#[cfg(test)]`, so a test-only helper mid-file hid every ask below it.
fn code_lines(source: &str) -> Vec<&str> {
    let lines: Vec<&str> = source.lines().collect();
    let test_module = |i: usize| {
        lines[i].trim_start().starts_with("#[cfg(test)]")
            && lines[i + 1..]
                .iter()
                .map(|l| l.trim_start())
                .find(|l| !l.is_empty() && !l.starts_with("#[") && !l.starts_with("//"))
                .is_some_and(|l| {
                    l.starts_with("mod ")
                        || l.starts_with("pub(crate) mod ")
                        || l.starts_with("pub mod ")
                })
    };
    let end = (0..lines.len())
        .find(|&i| test_module(i))
        .unwrap_or(lines.len());
    lines[..end]
        .iter()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect()
}

/// Every `Resource::<name>` on a line, with its argument list when it
/// has one on that line: `Resource::location()`, `Resource::new("place")`.
fn resource_tokens(line: &str) -> Vec<&str> {
    line.match_indices("Resource::")
        .map(|(at, needle)| {
            let rest = &line[at + needle.len()..];
            let name = rest
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            let after = &rest[name..];
            let args = if after.starts_with('(') {
                after.find(')').map_or(after.len(), |close| close + 1)
            } else {
                0
            };
            &line[at..at + needle.len() + name + args]
        })
        .collect()
}

/// FNV-1a, 64 bits, as hex: a stable digest with no dependency, and the
/// same answer on every host.
fn fnv(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// One file's raw-ask count and digest.
fn door_of(source: &str) -> (usize, String) {
    let lines = code_lines(source);
    let unnamed = unnamed_verb_checks(&lines);
    let mut count = 0;
    let mut parts: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let n = asks_on(line) + usize::from(unnamed.contains(&i));
        if n == 0 {
            continue;
        }
        count += n;
        parts.push(line.split_whitespace().collect::<Vec<_>>().join(" "));
        let from = i.saturating_sub(RESOURCE_WINDOW);
        let to = (i + RESOURCE_WINDOW).min(lines.len().saturating_sub(1));
        for near in &lines[from..=to] {
            parts.extend(resource_tokens(near).into_iter().map(str::to_string));
        }
    }
    (count, fnv(&parts.join("\n")))
}

/// Every non-test source file under crates/, by repo-relative path.
fn sources() -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !matches!(name.as_str(), "tests" | "benches" | "fuzz" | "target") {
                    walk(root, &path, out);
                }
            } else if name.ends_with(".rs") {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                out.insert(
                    rel.to_string_lossy().replace('\\', "/"),
                    std::fs::read_to_string(&path).unwrap_or_default(),
                );
            }
        }
    }
    let root = boss_testing::repo_root();
    let mut out = BTreeMap::new();
    walk(&root, &root.join("crates"), &mut out);
    out
}

/// The file that declares the controls, which names every verb once.
const CONTROLS_FILE: &str = "crates/core/boss-policy-client/src/controls.rs";

#[test]
fn a_door_asks_raw_only_where_its_resource_is_data() {
    let all = sources();
    boss_testing::assert_roster_floor!(all, 500, "source files the scan reads");
    let seen: BTreeMap<&str, (usize, String)> = all
        .iter()
        .filter(|(file, _)| file.as_str() != CONTROLS_FILE)
        .map(|(file, src)| (file.as_str(), door_of(src)))
        .filter(|(_, (n, _))| *n > 0)
        .collect();
    let declared: BTreeMap<&str, &RawAsk> = RAW_ASKS.iter().map(|r| (r.file, r)).collect();
    let mut wrong = Vec::new();
    for (file, (n, digest)) in &seen {
        match declared.get(file) {
            None => wrong.push(format!(
                "{file}: asks policy with a raw Action {n} time(s) — ask through the \
                 `boss_policy_client::controls` const for the pair (declare one there if the \
                 pair is new, which is what puts it in coverage); only a resource that is data \
                 is asked raw, declared in RAW_ASKS with `mentions: {n}`, `digest: \
                 \"{digest}\"` and why"
            )),
            Some(r) if r.mentions != *n || r.digest != digest => wrong.push(format!(
                "{file}: the scan sees {n} raw ask(s) with digest {digest}, RAW_ASKS declares \
                 {} with {} — a raw ask was added, moved, removed or changed, or the resource \
                 beside one; if the new ask is a static pair, ask through its const instead, \
                 else set `mentions: {n}` and `digest: \"{digest}\"`",
                r.mentions, r.digest
            )),
            Some(_) => {}
        }
    }
    for r in RAW_ASKS {
        assert!(!r.why.trim().is_empty(), "{}: a raw ask says why", r.file);
        if r.mentions > 0 && !seen.contains_key(r.file) {
            wrong.push(format!(
                "{}: RAW_ASKS declares {} raw ask(s) and the scan sees none — remove it",
                r.file, r.mentions
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}\n", wrong.join("\n"));
}

/// The name of every const `controls!` declares, read off its source.
fn declared_names(controls_src: &str) -> Vec<&str> {
    let body = controls_src
        .split("controls! {")
        .nth(1)
        .and_then(|b| b.split("#[cfg(test)]").next())
        .unwrap_or_default();
    body.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (name, rest) = l.split_once(": ")?;
            (!name.is_empty()
                && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
                && rest.contains('"'))
            .then_some(name)
        })
        .collect()
}

/// A declared control no door asks is a control coverage reports and
/// nothing checks — the reverse drift of a door no control declares.
#[test]
fn every_declared_control_is_asked_by_a_door() {
    let all = sources();
    let names = declared_names(&all[CONTROLS_FILE]);
    assert_eq!(
        names.len(),
        CONTROLS.len(),
        "the scan reads {} const names off controls.rs and CONTROLS holds {}",
        names.len(),
        CONTROLS.len()
    );
    let code: String = all
        .iter()
        .filter(|(file, _)| file.as_str() != CONTROLS_FILE)
        .flat_map(|(_, src)| code_lines(src))
        .collect::<Vec<_>>()
        .join("\n");
    let unasked: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !code.contains(&format!("controls::{n}")) && !code.contains(&format!("{n})")))
        .collect();
    assert!(
        unasked.is_empty(),
        "declared in controls.rs and asked by no door: {unasked:?} — remove the const, or \
         the door that should ask it"
    );
}

/// `scope_of` is always a read, so it names a `READ_` control — or the
/// `control` a helper was handed by callers that name one.
#[test]
fn every_scope_read_names_a_read_control() {
    let mut wrong = Vec::new();
    for (file, src) in sources() {
        for line in code_lines(&src) {
            for (at, _) in line.match_indices("scope_of(") {
                let call = &line[at..];
                let call = &call[..call.find(')').map_or(call.len(), |c| c + 1)];
                if call.contains("fn ") || line.contains("async fn scope_of") {
                    continue;
                }
                if !(call.contains("READ_") || call.ends_with(", control)")) {
                    wrong.push(format!("{file}: {}", line.trim()));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a scope read naming no READ_ control:\n{}",
        wrong.join("\n")
    );
}

/// The scan's own shape, so a change to it is a decision: a verb must
/// be `Action::<verb>` standing alone, not `CarAction::Retire` (the CLI's
/// clap enums were the triage's false positives) nor `Action::Reader`.
#[test]
fn the_scan_counts_a_policy_verb_and_not_a_lookalike() {
    assert_eq!(asks_on("check(user, Action::Create, r)"), 1);
    assert_eq!(asks_on("[Action::Create, Action::Update]"), 2);
    assert_eq!(asks_on("boss_policy_client::Action::Read,"), 1);
    assert_eq!(asks_on("CarAction::Retire { name } =>"), 0);
    assert_eq!(asks_on("Action::Reader"), 0);
    assert_eq!(asks_on("    .scope_predicate(user, Resource::job())"), 1);
    assert_eq!(asks_on("policy.ask(user, controls::CREATE_CLASS)"), 0);
    assert_eq!(
        door_of("x(Action::Read);\n// Action::Update\n#[cfg(test)]\nmod tests {\nAction::Close").0,
        1
    );
}

/// The four evasions the release review of car 2 found (005eb5b4,
/// LOW-1), each an ask the scan read as none. Folded into car 3 of
/// design 1c4e42e1.
#[test]
fn the_scan_sees_the_four_evasions_of_review_005eb5b4() {
    // 1. A const's verb with another resource: no `Action::` at all.
    assert_eq!(
        door_of("policy.check(user, CREATE_CLASS.action(), Resource::ledger())").0,
        1
    );
    assert_eq!(
        door_of(
            "    .check(\n        &user,\n        verb,\n        Resource::new(\"estate\"),\n    )"
        )
        .0,
        1,
        "the resource on a later line of the call"
    );
    // ... and not a check whose resource is data, nor one already
    // counted by its verb.
    assert_eq!(
        door_of("engine.check(&body.user, body.action, body.resource)").0,
        0
    );
    assert_eq!(door_of("p.check(u, Action::Read, Resource::job())").0, 1);
    // 2. Verbs brought in bare: every later `Create` is invisible.
    assert_eq!(asks_on("    use Action::*;"), 1);
    assert_eq!(asks_on("use Action::{Close, Create};"), 1);
    // 3. An ask below a test-only item in the MIDDLE of a file: the cut
    // is the test module, not the first `#[cfg(test)]`.
    assert_eq!(
        door_of("#[cfg(test)]\nfn helper() {}\nfn door() { x(Action::Read) }\n#[cfg(test)]\nmod tests {}").0,
        1
    );
    // 4. A verb parsed from text.
    assert_eq!(asks_on("let v = Action::from_str(\"create\")?;"), 1);
    assert_eq!(asks_on("let v: Action = s.parse::<Action>()?;"), 1);
}

/// Review L1, the mutation that stayed green under a count alone: the
/// same number of asks with the verb or the resource swapped. The digest
/// moves for either half, and not for reindenting the same line.
#[test]
fn a_swapped_ask_or_resource_moves_the_digest() {
    let door = "    match state\n        .policy\n        .check(&user, Action::Create, Resource::new(format!(\"job:{kind}\")))\n";
    let verb = door.replace("Action::Create", "Action::Retire");
    let noun = door.replace("job:{kind}", "place:{kind}");
    let reindented = door.replace("        .check", "            .check");
    let (n, d) = door_of(door);
    assert_eq!(n, 1);
    assert_eq!(door_of(&verb).0, 1, "the count alone cannot see it");
    assert_ne!(door_of(&verb).1, d, "a swapped verb moves the digest");
    assert_ne!(door_of(&noun).1, d, "a swapped resource moves the digest");
    assert_eq!(door_of(&reindented).1, d, "whitespace does not");
    assert_eq!(
        resource_tokens(r#"x(Resource::new("place"), Resource::job())"#),
        vec![r#"Resource::new("place")"#, "Resource::job()"]
    );
}

#[test]
fn the_const_names_are_read_off_the_declaring_macro() {
    let src = "controls! {\n    // a comment: not a const\n    /// doc\n    READ_JOB: Read \"job\";\n    CREATE_CLASS: Create \"class\", all;\n}\n#[cfg(test)]\nX: Y \"z\";";
    assert_eq!(declared_names(src), vec!["READ_JOB", "CREATE_CLASS"]);
}
