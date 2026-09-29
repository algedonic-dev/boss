//! Every door that asks policy is declared in `coverage::DOORS` — the
//! pin on the static control list (design 1c4e42e1 decision 8; backlog
//! 47aed706).
//!
//! WHY A PIN. A door's resource usually rides a helper, so no scan can
//! say WHICH pair a site asks — the triage resolved 135 sites by hand
//! (run 60497b5d). What a scan CAN see is that a site exists, and what
//! its lines say. So this reruns the triage's scan on the tree — every
//! `Action::<verb>`, `.scope_predicate(` and `asks!(` outside tests and
//! comments — and holds each file to its `Door`: the COUNT of asks
//! (`mentions`), and a DIGEST of every ask line, whitespace-collapsed,
//! plus every `Resource::…` token within three lines of one (`digest`).
//! The count alone let an ask be swapped for another in place — the
//! review of this car changed `Create`/`location` to `Retire`/`"place"`
//! in boss-locations and the pin stayed green (L1) — so any edit to an
//! ask line or to the resource beside it now fails, naming the file:
//! resolve what the site asks now, correct that door's `asks` (which is
//! every static control coverage reads), and set the two values the
//! failure prints. A comment asking the next author to keep a list in
//! step is not a mechanism (CLAUDE.md §9a); this is the holding action
//! until every door asks through a `Control` const and the compiler pins
//! it. What it still cannot see: a resource changed inside a helper more
//! than three lines from its ask, and an action held in a variable.
//!
//! tree-wide pin — it scans every crate's source under crates/, so a door
//! added in any other crate changes its verdict.

use std::collections::BTreeMap;
use std::path::Path;

use boss_policy_client::coverage::DOORS;

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

/// How many policy asks the scan sees on one line of code.
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
    let reads = line.matches(".scope_predicate(").count();
    let macros = line
        .match_indices("asks!(")
        .filter(|(at, _)| alone_before(*at))
        .count();
    verbs + reads + macros
}

/// The code lines of one source file the scan reads: everything before
/// its first `#[cfg(test)]`, with a comment line blanked (kept, so the
/// resource window counts real lines) — the triage's own cut.
fn code_lines(source: &str) -> Vec<&str> {
    source
        .lines()
        .take_while(|l| !l.trim_start().starts_with("#[cfg(test)]"))
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

/// One file's ask count and digest.
fn door_of(source: &str) -> (usize, String) {
    let lines = code_lines(source);
    let mut count = 0;
    let mut parts: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let n = asks_on(line);
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

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (usize, String)>) {
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
            let door = door_of(&std::fs::read_to_string(&path).unwrap_or_default());
            if door.0 > 0 {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                out.insert(rel.to_string_lossy().replace('\\', "/"), door);
            }
        }
    }
}

fn scanned() -> BTreeMap<String, (usize, String)> {
    let root = boss_testing::repo_root();
    let mut out = BTreeMap::new();
    walk(&root, &root.join("crates"), &mut out);
    out
}

#[test]
fn every_door_file_is_declared_with_the_asks_the_scan_sees() {
    let seen = scanned();
    boss_testing::assert_roster_floor!(seen, 30, "files the scan finds asking policy");
    let declared: BTreeMap<&str, (usize, &str)> = DOORS
        .iter()
        .map(|d| (d.file, (d.mentions, d.digest)))
        .collect();
    let mut wrong = Vec::new();
    for (file, (n, digest)) in &seen {
        match declared.get(file.as_str()) {
            None => wrong.push(format!(
                "{file}: asks policy {n} time(s) and is not a Door in coverage::DOORS — resolve \
                 each ask to its (action, resource), declare the file with those `asks`, \
                 `mentions: {n}` and `digest: \"{digest}\"`"
            )),
            Some((m, d)) if m != n || d != digest => wrong.push(format!(
                "{file}: the scan sees {n} ask(s) with digest {digest}, DOORS declares {m} with \
                 {d} — an ask was added, moved, removed or changed, or the resource beside one; \
                 resolve what the changed site asks, correct this door's `asks`, and set \
                 `mentions: {n}` and `digest: \"{digest}\"`"
            )),
            Some(_) => {}
        }
    }
    for (file, (m, _)) in &declared {
        if !seen.contains_key(*file) {
            wrong.push(format!(
                "{file}: DOORS declares {m} ask(s) and the scan sees none — the door is gone; \
                 remove it, and any pair only it asked, from coverage::DOORS"
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}\n", wrong.join("\n"));
}

#[test]
fn a_door_file_is_declared_once() {
    let mut files: Vec<&str> = DOORS.iter().map(|d| d.file).collect();
    let n = files.len();
    files.sort_unstable();
    files.dedup();
    assert_eq!(
        files.len(),
        n,
        "a file declared twice would be counted twice"
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
    assert_eq!(asks_on("use Action::{Close, Create};"), 0);
    assert_eq!(asks_on("    .scope_predicate(user, Resource::job())"), 1);
    assert_eq!(asks_on("asks!(LedgerCreate, Create, ledger);"), 1);
    assert_eq!(
        door_of("x(Action::Read);\n// Action::Update\n#[cfg(test)]\nAction::Close").0,
        1
    );
}

/// Review L1, the mutation that stayed green: the same number of asks
/// in boss-locations, with the verb and the resource swapped. The digest
/// moves for either half, and not for reindenting the same line.
#[test]
fn a_swapped_ask_or_resource_moves_the_digest() {
    let door = "    match state\n        .policy\n        .check(&user, Action::Create, Resource::location())\n";
    let verb = door.replace("Action::Create", "Action::Retire");
    let noun = door.replace("Resource::location()", "Resource::new(\"place\")");
    let reindented = door.replace("        .check", "            .check");
    let (n, d) = door_of(door);
    assert_eq!(n, 1);
    assert_eq!(door_of(&verb).0, 1, "the count alone cannot see it");
    assert_ne!(door_of(&verb).1, d, "a swapped verb moves the digest");
    assert_ne!(door_of(&noun).1, d, "a swapped resource moves the digest");
    assert_eq!(door_of(&reindented).1, d, "whitespace does not");
    // The resource may sit a line or three from its ask.
    let split = "authorize(\n    &state,\n    Resource::location(),\n    Action::Create,\n)";
    let moved = split.replace("location()", "new(\"place\")");
    assert_ne!(door_of(split).1, door_of(&moved).1);
    assert_eq!(
        resource_tokens(r#"x(Resource::new("place"), Resource::job())"#),
        vec![r#"Resource::new("place")"#, "Resource::job()"]
    );
}
