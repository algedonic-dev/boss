//! No client bakes the machine token into its default headers, except
//! the callers named below, and that list only shrinks (design 6805c764,
//! car 2; backlog 2710c8fc).
//!
//! WHAT WAS MEASURED (adversarial review of car 2 slice 1, 22378668,
//! 2026-09-26, finding S1). Seven clients read the token ONCE, at build,
//! into `default_headers` — `http_client::base`, the assignment
//! dispatcher, the handlers' `api_client`, the conductor, boss-sim, the
//! workflow bootstrap and the escalation router — so a client built at
//! boot sent the boot-time token for the life of its process. The broker
//! rotates by moving `current` to `previous` and then revoking
//! `previous` (car 3); every such caller would have been refused at an
//! enforcing gate the moment the old value was revoked, until someone
//! restarted it. The file source was chosen precisely so a rotation
//! would not need that restart (design choice 2).
//!
//! THE FIX, AND WHAT THIS HOLDS. `boss_core::machine_token::Client`
//! stamps the process's one watched `Source` on every request it builds.
//! `machine_token::attach` — the read-once door — survives only for the
//! callers in [`BAKED`], each with why it has not moved yet. A new call
//! is refused, naming its file; a row whose file no longer calls it is
//! refused as stale, so converting a caller forces the row's deletion
//! and the exemption cannot outlive its reason.
//!
//! EVERY SPELLING (review of 6fbc7fc7, 2026-09-28, follow-up 2). The
//! first version matched `machine_token::attach(` only, so a file that
//! imported the door (`use boss_core::machine_token::attach;`, or under
//! another name) and called it bare passed, and so did one that skipped
//! the door and put `machine_token::HEADER` into a header map itself.
//! Both are read here now: every name a file gives the door or the
//! module, and every production line that spells the header — by the
//! constant under any name, or as the literal — outside [`SPELLERS`],
//! the few places whose job is the header itself.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::production_source::production_text;
use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

/// Files that still bake the token in, and why each has not moved.
/// EMPTY since car 2's blocking-senders slice (2026-09-29): boss-sim's
/// live output and the workflow bootstrap walk, the last two, now stamp
/// through `machine_token::BlockingClient`, and the read-once door they
/// called, `machine_token::attach`, is deleted — so a new caller does
/// not compile, and this scan is the guard should it ever come back.
///
/// It held four when it was written (6fbc7fc7) and went one slice at a
/// time: the dispatcher handlers' `api_client` (car 2's handlers slice),
/// the conductor's client (the CLI slice), boss-sim's output and the
/// workflow bootstrap (the blocking-senders slice). Its ceiling was the
/// guard against a row being ADDED beside a new caller (review of
/// 6fbc7fc7, follow-up 2); at zero the ceiling is simply "empty", which
/// the pin asserts, so a row can no longer be added at all.
const BAKED: &[(&str, &str)] = &[];

/// Production files whose job is the header itself, and why. Everything
/// else reaches it through a stamping client — `machine_token::Client`
/// (and `http_client::base`), or the gateway's `MachineClient` — which
/// carries the token on every request and follows no redirect.
const SPELLERS: &[(&str, &str)] = &[
    (
        "crates/core/boss-core/src/machine_token.rs",
        "the definition, and the stamp every machine client applies.",
    ),
    (
        "crates/core/boss-core/src/machine_gate.rs",
        "the gate READS the header off an inbound request; it sends nothing.",
    ),
    (
        "crates/core/boss-gateway/src/machine_client.rs",
        "the gateway's reqwest-0.13 twin of machine_token::Client — the stamp, with \
         redirects off.",
    ),
    (
        "crates/core/boss-gateway/src/role_headers.rs",
        "the gateway's forward stamp: an INBOUND request gains the token after the edge \
         strip, and the reverse-proxy client that sends it on has redirects off (main.rs).",
    ),
];

const DEFINITION: &str = "crates/core/boss-core/src/machine_token.rs";
const THIS_FILE: &str = "/tests/no_client_bakes_the_machine_token_in.rs";

fn walk(dir: &Path, skip_tests: bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| {
                n == "target"
                    || n == "node_modules"
                    || (skip_tests && (n == "tests" || n == "benches"))
            }) {
                continue;
            }
            walk(&p, skip_tests, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn rel(root: &Path, p: &Path) -> Option<String> {
    Some(
        p.strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

/// The names a file reaches the module and its items by. Every `use`
/// statement that mentions `machine_token` is read whole (it may span
/// lines), and an `as` rename is followed — `use boss_core::machine_token
/// as mt;` makes `mt::attach(` a call, `use …::{attach as bake}` makes
/// `bake(` one. A glob (`machine_token::*`) imports every item under its
/// own name, and `{self as mt}` aliases the module like `as mt` does —
/// both passed the first version of this scan (review of 39949355, M5
/// and M6, 2026-09-28).
struct Names {
    /// `machine_token` and every alias of the module.
    module: Vec<String>,
    /// Bare names a hand-stamping door (`attach`, and `stamp` should it
    /// ever be public again) was imported under.
    doors: Vec<String>,
    /// Bare names `HEADER` was imported under.
    header: Vec<String>,
}

/// The module's items that put the token on a request by hand. `stamp`
/// is private now (review M4), but a scan that forgot it would pass the
/// day someone made it public again.
const DOORS: &[&str] = &["attach", "stamp"];

fn names(src: &str) -> Names {
    let uses = Regex::new(r"(?s)\buse\s+[^;]*;").unwrap();
    let ident = r"([A-Za-z_][A-Za-z0-9_]*)";
    let module_alias = Regex::new(&format!(r"\bmachine_token\s+as\s+{ident}")).unwrap();
    let self_alias = Regex::new(&format!(r"\bself\s+as\s+{ident}")).unwrap();
    let glob = Regex::new(r"\bmachine_token\s*::\s*\*").unwrap();
    let item = |name: &str| Regex::new(&format!(r"\b{name}\b(?:\s+as\s+{ident})?")).unwrap();
    let header_re = item("HEADER");
    let door_res: Vec<Regex> = DOORS.iter().map(|d| item(d)).collect();
    let mut n = Names {
        module: vec!["machine_token".to_string()],
        doors: Vec::new(),
        header: Vec::new(),
    };
    let named = |c: regex::Captures| {
        c.get(1)
            .map_or_else(|| c[0].to_string(), |m| m.as_str().to_string())
    };
    for u in uses.find_iter(src).map(|m| m.as_str()) {
        if !u.contains("machine_token") {
            continue;
        }
        n.module
            .extend(module_alias.captures_iter(u).map(|c| c[1].to_string()));
        n.module
            .extend(self_alias.captures_iter(u).map(|c| c[1].to_string()));
        if glob.is_match(u) {
            n.doors.extend(DOORS.iter().map(|d| d.to_string()));
            n.header.push("HEADER".to_string());
        }
        for re in &door_res {
            n.doors.extend(re.captures_iter(u).map(named));
        }
        n.header.extend(header_re.captures_iter(u).map(named));
    }
    n
}

/// Code lines: not a comment, not a `use` line (a `use` names a thing,
/// it does not call or send it).
fn code_lines(src: &str) -> impl Iterator<Item = &str> {
    src.lines().filter(|l| {
        let t = l.trim_start();
        !t.starts_with("//") && !t.starts_with("use ") && !t.starts_with("pub use ")
    })
}

/// Lines that call the read-once door, by any name the file gives it.
fn calls(src: &str) -> usize {
    let n = names(src);
    let bare: Vec<Regex> = n
        .doors
        .iter()
        .map(|a| Regex::new(&format!(r"(?:^|[^A-Za-z0-9_:.]){a}\s*\(")).unwrap())
        .collect();
    code_lines(src)
        .filter(|l| {
            n.module
                .iter()
                .any(|m| DOORS.iter().any(|d| l.contains(&format!("{m}::{d}("))))
                || bare.iter().any(|r| r.is_match(l))
        })
        .count()
}

/// Lines that spell the header: the literal, the constant through the
/// module under any name, or the constant under any name it was
/// imported as.
fn spells_header(src: &str) -> Vec<usize> {
    let n = names(src);
    let bare: Vec<Regex> = n
        .header
        .iter()
        .map(|h| Regex::new(&format!(r"(?:^|[^A-Za-z0-9_:]){h}\b")).unwrap())
        .collect();
    src.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with("//") && !t.starts_with("use ") && !t.starts_with("pub use ")
        })
        .filter(|(_, l)| {
            l.contains("\"x-boss-machine-token\"")
                || n.module.iter().any(|m| l.contains(&format!("{m}::HEADER")))
                || bare.iter().any(|r| r.is_match(l))
        })
        .map(|(i, _)| i + 1)
        .collect()
}

fn callers(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(&root.join("crates"), false, &mut files);
    let mut out: Vec<String> = files
        .iter()
        .filter_map(|p| {
            let rel = rel(root, p)?;
            if rel == DEFINITION || rel.ends_with(THIS_FILE) {
                return None;
            }
            let src = std::fs::read_to_string(p).ok()?;
            (calls(&src) > 0).then_some(rel)
        })
        .collect();
    out.sort();
    out
}

/// Production files (tests directories skipped, `#[cfg(test)]` items
/// blanked) that spell the header, with the lines.
fn spellers(root: &Path) -> Vec<(String, Vec<usize>)> {
    let mut files = Vec::new();
    walk(&root.join("crates"), true, &mut files);
    let mut out: Vec<(String, Vec<usize>)> = files
        .iter()
        .filter_map(|p| {
            let rel = rel(root, p)?;
            let src = std::fs::read_to_string(p).ok()?;
            let prod = production_text(&src)
                .unwrap_or_else(|e| panic!("{rel} does not parse as Rust: {e}"));
            let lines = spells_header(&prod);
            (!lines.is_empty()).then_some((rel, lines))
        })
        .collect();
    out.sort();
    out
}

#[test]
fn no_client_bakes_the_machine_token_in() {
    let root = repo_root();
    let found = callers(&root);
    let named: Vec<&str> = BAKED.iter().map(|(f, _)| *f).collect();

    assert!(
        BAKED.is_empty(),
        "BAKED holds {} rows and it only shrinks — it reached empty on 2026-09-29, and the door \
         it exempted callers of, `machine_token::attach`, is deleted. Build the new caller's \
         client as `boss_core::machine_token::Client` (or `BlockingClient`) instead of adding a \
         row.",
        BAKED.len()
    );

    let new: Vec<&String> = found
        .iter()
        .filter(|f| !named.contains(&f.as_str()))
        .collect();
    assert!(
        new.is_empty(),
        "these files bake the machine token into a client with `machine_token::attach` (under \
         some name), so a client built at boot sends the boot-time token until its process \
         restarts, and the broker's revoke of `previous` refuses it (design 6805c764 car 2, \
         review S1). Build the client as `boss_core::machine_token::Client` (or \
         `boss_core::http_client::base`), which stamps the watched value on every request:\n  {}",
        new.iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );

    let stale: Vec<&str> = named
        .iter()
        .filter(|f| !found.iter().any(|g| g == *f))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "BAKED names files that no longer call `machine_token::attach` — delete their rows \
         and lower BAKED_CEILING, the list only shrinks:\n  {}",
        stale.join("\n  ")
    );

    // An exemption is not a leak (review of 39949355, 2026-09-28): a
    // client that still bakes the token into its default headers must at
    // least refuse redirects, or a 302 carries the token to the host it
    // names.
    let unsafe_rows: Vec<String> = named
        .iter()
        .filter_map(|f| {
            let src = std::fs::read_to_string(root.join(f)).unwrap_or_default();
            let prod = production_text(&src).unwrap_or(src);
            baked_file_refusal(&prod).map(|why| format!("{f}: {why}"))
        })
        .collect();
    assert!(
        unsafe_rows.is_empty(),
        "these BAKED files build a token-carrying client the exemption does not cover:\n  {}",
        unsafe_rows.join("\n  ")
    );
}

/// Why a [`BAKED`] file's production text is refused, or `None`.
///
/// The redirect check reads the whole file — any line spelling
/// `redirect::Policy::none()` — so it vouches for ONE builder, not for
/// every builder in the file: a second `attach(` client without the
/// policy, beside the compliant one, passed (review of
/// fix/a-machine-client-follows-no-redirect, finding 2; backlog
/// ee96c839). An exemption is for the one client its row names, so a
/// BAKED file holds exactly one call of the door, and the file-level
/// policy check is then a check of that one builder.
fn baked_file_refusal(prod: &str) -> Option<String> {
    let n = calls(prod);
    if n > 1 {
        return Some(format!(
            "{n} calls of the read-once door; its row exempts ONE client, and the redirect \
             check below cannot tell which builder carries `Policy::none()`. Build the \
             second client as `boss_core::machine_token::Client` instead"
        ));
    }
    if !code_lines(prod).any(|l| l.contains("redirect::Policy::none()")) {
        return Some(
            "may follow a redirect — add `.redirect(reqwest::redirect::Policy::none())` to \
             its builder"
                .to_string(),
        );
    }
    None
}

#[test]
fn a_baked_file_holds_one_client_and_it_refuses_redirects() {
    let compliant = "pub fn api_client() -> reqwest::Client {\n\
         \x20   let mut h = HeaderMap::new();\n\
         \x20   boss_core::machine_token::attach(&mut h);\n\
         \x20   reqwest::Client::builder().default_headers(h)\n\
         \x20       .redirect(reqwest::redirect::Policy::none()).build().unwrap()\n\
         }\n";
    assert_eq!(baked_file_refusal(compliant), None, "the control");

    // The review's shape: a second baked builder with no policy, beside
    // the compliant one — the file still spells `Policy::none()` once.
    let second = format!(
        "{compliant}pub fn partner_client() -> reqwest::Client {{\n\
         \x20   let mut h = HeaderMap::new();\n\
         \x20   boss_core::machine_token::attach(&mut h);\n\
         \x20   reqwest::Client::builder().default_headers(h).build().unwrap()\n\
         }}\n"
    );
    let why = baked_file_refusal(&second).expect("a second baked client must be refused");
    assert!(why.starts_with("2 calls"), "{why}");

    let following = compliant.replace(".redirect(reqwest::redirect::Policy::none())", "");
    let why = baked_file_refusal(&following).expect("a following client must be refused");
    assert!(why.contains("redirect"), "{why}");
}

#[test]
fn only_the_stamping_doors_spell_the_header() {
    let root = repo_root();
    let found = spellers(&root);
    let named: Vec<&str> = SPELLERS.iter().map(|(f, _)| *f).collect();

    let new: Vec<String> = found
        .iter()
        .filter(|(f, _)| !named.contains(&f.as_str()))
        .map(|(f, lines)| format!("{f}:{lines:?}"))
        .collect();
    assert!(
        new.is_empty(),
        "these production files put the machine token header on a request themselves, \
         outside the stamping clients. A hand-built header rides whatever client it is \
         handed — one whose redirects are on hands the estate token to the host a 302 names \
         (review of 6fbc7fc7, finding 1), and one built once bakes a value a rotation \
         revokes (review S1). Send through `boss_core::machine_token::Client` / \
         `http_client::base`, or the gateway's `MachineClient`:\n  {}",
        new.join("\n  ")
    );

    let stale: Vec<&str> = named
        .iter()
        .filter(|f| !found.iter().any(|(g, _)| g == *f))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "SPELLERS names files that no longer spell the header — delete their rows:\n  {}",
        stale.join("\n  ")
    );
}

#[test]
fn the_scan_sees_every_spelling_and_ignores_a_comment() {
    // The controls: a scan that matched nothing would pass the pins
    // above on any tree.
    assert_eq!(calls("    boss_core::machine_token::attach(&mut h);\n"), 1);
    assert_eq!(
        calls("    // boss_core::machine_token::attach(&mut h);\n"),
        0
    );
    // Imported bare, imported renamed, the module renamed.
    assert_eq!(
        calls("use boss_core::machine_token::attach;\nfn f() { attach(&mut h); }\n"),
        1
    );
    assert_eq!(
        calls(
            "use boss_core::machine_token::{\n    attach as bake,\n    HEADER,\n};\nfn f() { bake(&mut h); }\n"
        ),
        1
    );
    assert_eq!(
        calls("use boss_core::machine_token as mt;\nfn f() { mt::attach(&mut h); }\n"),
        1
    );
    // A method of the same name on something else is not the door.
    assert_eq!(
        calls("use boss_core::machine_token::attach;\nfn f() { x.attach(1); }\n"),
        0
    );
    // The review of 39949355's mutants, each of which passed the first
    // version (M4: the then-public `stamp`; M5: a glob import; M6:
    // `self as` inside a use).
    assert_eq!(
        calls("fn f() { machine_token::stamp(reqwest::Client::new().get(u), &shared()); }\n"),
        1,
        "M4"
    );
    assert_eq!(
        calls("use boss_core::machine_token::*;\nfn f() { attach(&mut h); }\n"),
        1,
        "M5, the door"
    );
    assert_eq!(
        spells_header("use boss_core::machine_token::*;\nfn f() { h.insert(HEADER, v); }\n"),
        vec![2],
        "M5, the header"
    );
    assert_eq!(
        spells_header(
            "use boss_core::machine_token::{self as mt};\nfn f() { rb.header(mt::HEADER, v) }\n"
        ),
        vec![2],
        "M6"
    );
    assert_eq!(
        calls(
            "use boss_core::machine_token::{self as mt, Source};\nfn f() { mt::attach(&mut h); }\n"
        ),
        1,
        "M6, the door"
    );

    assert_eq!(
        spells_header("h.insert(boss_core::machine_token::HEADER, v);\n"),
        vec![1]
    );
    assert_eq!(
        spells_header("h.insert(\"x-boss-machine-token\", v);\n"),
        vec![1]
    );
    assert_eq!(
        spells_header(
            "use boss_core::machine_token::HEADER as TOK;\nfn f() { rb.header(TOK, v) }\n"
        ),
        vec![2]
    );
    assert_eq!(
        spells_header("// machine_token::HEADER is the name\n"),
        Vec::<usize>::new()
    );

    let root = repo_root();
    assert!(
        callers(&root).len() >= BAKED.len(),
        "the scan found fewer callers than BAKED names — it is not reading the tree"
    );
    assert!(
        spellers(&root).len() >= SPELLERS.len(),
        "the scan found fewer spellers than SPELLERS names — it is not reading the tree"
    );
}
