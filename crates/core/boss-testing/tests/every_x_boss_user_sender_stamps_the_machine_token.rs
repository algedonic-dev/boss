//! Every production file that sends `x-boss-user` sends it through a
//! client that stamps the machine token — except the files named in
//! [`NOT_YET`], each with why, and that list only shrinks (design
//! 6805c764, car 2; backlog 2710c8fc).
//!
//! WHAT WAS MEASURED (adversarial review of car 2 slice 1, 22378668,
//! 2026-09-26, finding S2). The machine door's gate, once a port is set
//! to `enforce`, refuses any request that does not carry the estate
//! token. The callers that assert an identity with `x-boss-user` are the
//! machine callers the design names; about thirty files did so on a
//! plain `reqwest::Client` with no token at all — every policy check,
//! the gateway's login lookup and page flights read, the dispatcher's
//! sub-job spawn and reorder helpers, and more. Each one would have
//! become a 401 the minute its target port enforced, which is how a
//! system of record goes dark on a flag flip.
//!
//! WHAT THIS HOLDS. A file whose production code (tests directories
//! skipped, `#[cfg(test)]` items blanked) puts `x-boss-user` on a
//! request must also name a stamping door: `machine_token::Client` (or
//! its blocking twin, `machine_token::BlockingClient`),
//! `http_client::base`, the gateway's `MachineClient`, or the door the
//! sibling pin `no_client_bakes_the_machine_token_in` governs
//! (`machine_token::HEADER`; its read-once `attach` was deleted with the
//! last baked client, 2026-09-29). A file that does
//! not is refused, naming it and its lines; a [`NOT_YET`] row whose file
//! no longer sends the header, or now stamps it, is refused as stale; and
//! the list may not grow past [`NOT_YET_CEILING`].
//!
//! WHAT IT DOES NOT HOLD, said so it is not assumed. It is FILE-level: a
//! file with one stamping client and a second plain one sending the
//! header passes. And a sender is found by the header's NAME on or beside
//! a `.header(` / `insert(` line — the literal, in any case, or a `const`
//! the file binds to it — so a header name built at run time is not
//! seen. The one gap closed by type rather than by this scan is the
//! redirect: every stamping client refuses to follow one.
//!
//! WHAT IT SKIPS, AND THE PIN THAT MAKES THE SKIP SAFE. The walk skips
//! every directory named `tests`, which is right for a crate's
//! integration tests and right inside `src/` only while a `#[cfg(test)]`
//! module is the one way in. The broker-stage car put the first file
//! there (`credentials/http/tests/stage.rs`, by `#[path]`), and its
//! review asked for a pin that it stays so (25fff647, F6; backlog
//! 3ecb415b): [`every_file_under_a_src_tests_directory_is_reached_only_through_cfg_test`].
//! Without it, production code in such a directory would be a sender
//! this scan never reads.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::production_source::{production_text, without_comments};
use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

/// Files that send `x-boss-user` with no stamping door yet, and why.
/// The blocking senders — the policy and operator-baseline bootstraps,
/// boss-sim's workforce, the brewery's prepare walk — left it in car 2's
/// blocking-senders slice (2026-09-29), onto
/// `machine_token::BlockingClient`. The last relay, boss-simulator,
/// now forwards only its inbound gate's VerifiedEstate credential;
/// admitting an assertion under report never stamps it (2710c8fc R5).
const NOT_YET: &[(&str, &str)] = &[];

/// The most [`NOT_YET`] may hold: its length when it was written (41),
/// less the 23 dispatcher handlers that now hold their jobs-API client
/// as a `machine_token::Client` (car 2's handlers slice), less the 11
/// boss verbs whose jobs-API helpers now take one, the conductor's with
/// them (car 2's CLI slice), less the six blocking senders (car 2's
/// blocking-senders slice, 2026-09-29), and the simulator relay (R5).
/// Lower it as rows go; never
/// raise it.
const NOT_YET_CEILING: usize = 0;

/// Names that mean the file stamps the token on what it sends.
const STAMPING_DOORS: &[&str] = &[
    "RelayClient",
    "machine_token::Client",
    "machine_token::BlockingClient",
    "BlockingHttp",
    // boss-cli's one constructor of a `machine_token::Client` (and
    // `machine_client_with`), which every boss verb builds its client by.
    "sor_client::machine_client",
    "http_client::base",
    "MachineClient",
    "machine_token::HEADER",
];

/// Not network senders, by path prefix, and why. Each puts the header on
/// a request that never leaves the process.
const NOT_SENDERS: &[(&str, &str)] = &[
    (
        "crates/core/boss-testing/src/",
        "test support: `TestRequest` builds in-process requests for a router's `oneshot`.",
    ),
    (
        "crates/core/boss-jobs/src/agents/door.rs",
        "the login door: a middleware rewriting the INBOUND request's x-boss-user id to the \
         agent it aliases, before the handler reads it.",
    ),
    (
        "crates/core/boss-core/src/machine_gate.rs",
        "the machine gate: on a probe-reader match it rewrites the INBOUND request's \
         x-boss-user to the probe reader before any handler reads it (design b35c22b4, Q2). \
         It sends nothing; listed so it is not passed by naming machine_token::HEADER, which \
         it reads rather than stamps.",
    ),
    (
        "crates/core/boss-jobs/src/runner_credential.rs",
        "the credential resolver: it replaces the INBOUND request's asserted actor with \
         the credential's actor before the handler reads it (design f623e425 option A). \
         It sends nothing; stamping an unused outbound client would conceal this distinction.",
    ),
];

/// Middleware classification holds only while the file contains no
/// outgoing client or send. A later sender must return to the scan,
/// not inherit a file-level exclusion (gate 41371f4b, 2026-10-05).
fn not_a_network_sender(path: &str, source: &str) -> bool {
    let Some((prefix, _)) = NOT_SENDERS
        .iter()
        .find(|(prefix, _)| path.starts_with(prefix))
    else {
        return false;
    };
    if prefix.ends_with('/') {
        return true; // In-process test support, not a production middleware.
    }
    let code = without_comments(source);
    let outgoing = Regex::new(
        r"reqwest\s*::|machine_token\s*::\s*(?:Blocking)?Client\b|http_client\s*::|\bMachineClient\b|\.(?:send|execute)\s*\(",
    )
    .unwrap();
    !outgoing.is_match(&code)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| {
                n == "target" || n == "node_modules" || n == "tests" || n == "benches"
            }) {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The 1-based lines of `src` that put `x-boss-user` on a request: the
/// name (the literal in any case, or a `const` bound to it) on a
/// non-comment line whose own line or a neighbour builds a header —
/// `.header(`, `.headers(`, `insert(` — and that is not a read of one.
fn sends_identity(src: &str) -> Vec<usize> {
    let bound = Regex::new(
        r#"(?i)const\s+([A-Z_][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*"x-boss-user""#,
    )
    .unwrap();
    let mut spelled: Vec<String> = vec![r#"(?i:"x-boss-user")"#.to_string()];
    spelled.extend(bound.captures_iter(src).map(|c| format!(r"\b{}\b", &c[1])));
    let names: Vec<Regex> = spelled.iter().map(|s| Regex::new(s).unwrap()).collect();
    // A READ names the header as `get`'s argument: `headers.get(NAME)`.
    // `client.get(&url).header(NAME, …)` on the same line is a send.
    let reads: Vec<Regex> = spelled
        .iter()
        .map(|s| Regex::new(&format!(r"\.get(?:_all)?\(\s*{s}")).unwrap())
        .collect();
    let lines: Vec<&str> = src.lines().collect();
    let builds = |i: usize| {
        let lo = i.saturating_sub(1);
        let hi = (i + 2).min(lines.len());
        lines[lo..hi]
            .iter()
            .any(|l| l.contains(".header(") || l.contains(".headers(") || l.contains("insert("))
    };
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with("//") && !t.starts_with("const ") && !t.starts_with("pub const ")
        })
        .filter(|(_, l)| names.iter().any(|r| r.is_match(l)))
        .filter(|(_, l)| !reads.iter().any(|r| r.is_match(l)))
        .filter(|(i, _)| builds(*i))
        .map(|(i, _)| i + 1)
        .collect()
}

/// Does the CODE name a stamping door? Comments are stripped first: a
/// comment saying `MachineClient` satisfied this (review of 39949355,
/// M7).
fn stamps(src: &str) -> bool {
    let code = without_comments(src);
    STAMPING_DOORS.iter().any(|d| code.contains(d))
}

/// Every production file that sends the header: (path, lines, stamps).
fn senders(root: &Path) -> Vec<(String, Vec<usize>, bool)> {
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let mut out: Vec<(String, Vec<usize>, bool)> = files
        .iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(p).ok()?;
            let prod = production_text(&src)
                .unwrap_or_else(|e| panic!("{rel} does not parse as Rust: {e}"));
            if not_a_network_sender(&rel, &prod) {
                return None;
            }
            let lines = sends_identity(&prod);
            (!lines.is_empty()).then(|| (rel, lines, stamps(&prod)))
        })
        .collect();
    out.sort();
    out
}

#[test]
fn every_x_boss_user_sender_stamps_the_machine_token() {
    let root = repo_root();
    let found = senders(&root);
    let named: Vec<&str> = NOT_YET.iter().map(|(f, _)| *f).collect();

    assert_eq!(
        NOT_YET.len(),
        NOT_YET_CEILING,
        "NOT_YET holds {} rows and its ceiling is {NOT_YET_CEILING}: the list only shrinks. \
         Send through a stamping client instead of adding a row.",
        NOT_YET.len()
    );

    let unstamped: Vec<String> = found
        .iter()
        .filter(|(f, _, stamped)| !stamped && !named.contains(&f.as_str()))
        .map(|(f, lines, _)| format!("{f}:{lines:?}"))
        .collect();
    assert!(
        unstamped.is_empty(),
        "these files send x-boss-user on a client that carries no machine token, so the \
         port they reach refuses them the moment its gate enforces (design 6805c764 car 2, \
         review S2). Send through `boss_core::machine_token::Client` (or \
         `http_client::base`; the gateway's `MachineClient`) — never a client that also \
         calls a third party:\n  {}",
        unstamped.join("\n  ")
    );

    let stale: Vec<String> = named
        .iter()
        .filter_map(|f| match found.iter().find(|(g, _, _)| g == f) {
            None => Some(format!("{f} (sends no x-boss-user now)")),
            Some((_, _, true)) => Some(format!("{f} (stamps now)")),
            Some(_) => None,
        })
        .collect();
    assert!(
        stale.is_empty(),
        "NOT_YET names files that no longer need the exemption — delete their rows and \
         lower NOT_YET_CEILING:\n  {}",
        stale.join("\n  ")
    );
}

#[test]
fn the_scan_sees_a_sender_and_not_a_reader() {
    // Controls: a scan that saw nothing would pass the pin on any tree.
    assert_eq!(
        sends_identity("    .header(\"x-boss-user\", who)\n"),
        vec![1]
    );
    assert_eq!(
        sends_identity("    rb.header(\n        \"X-Boss-User\",\n        who,\n    )\n"),
        vec![2]
    );
    assert_eq!(
        sends_identity("    h.insert(\"x-boss-user\", v);\n"),
        vec![1]
    );
    // A GET that sends the header is a send (the CLI's verbs).
    assert_eq!(
        sends_identity("    client.get(&url).header(\"x-boss-user\", user.as_str())\n"),
        vec![1]
    );
    assert_eq!(
        sends_identity("const WHO: &str = \"x-boss-user\";\nfn f() { rb.header(WHO, v) }\n"),
        vec![2]
    );
    // A read, and a comment, are not sends.
    assert_eq!(
        sends_identity("    let u = headers.get(\"x-boss-user\");\n    rb.header(\"a\", b)\n"),
        Vec::<usize>::new()
    );
    assert_eq!(
        sends_identity("    // .header(\"x-boss-user\", who)\n"),
        Vec::<usize>::new()
    );
    assert!(stamps("    http: boss_core::machine_token::Client,\n"));
    assert!(!stamps("    http: reqwest::Client,\n"));
    // M7 (review of 39949355): a comment naming a door is not a door.
    assert!(!stamps(
        "    // TODO: move to MachineClient\n    http: reqwest::Client, /* machine_token::Client */\n"
    ));

    let found = senders(&repo_root());
    assert!(
        found.iter().filter(|(_, _, s)| *s).count() >= 10,
        "the scan found under ten stamping senders — it is not reading the tree: {found:?}"
    );
}

// ----- the `tests` directories under `src/` the walk skips (review of
// 25fff647, F6; backlog 3ecb415b) -----
//
// WHAT IT DOES NOT HOLD, said so it is not assumed: a `#[path]` is
// resolved against the declaring file's directory, which is rustc's rule
// for a file's top-level items and not for one inside an inline module;
// a `#[path]` on the `tests` module itself is not followed; and `benches`
// directories, which the walk also skips, are not judged here.

/// Every `.rs` under `dir`, nothing skipped but build output.
fn every_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules")
            {
                continue;
            }
            every_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The first directory named `tests` strictly below a `src` component
/// of `path`, when there is one.
fn src_tests_dir(path: &Path) -> Option<PathBuf> {
    let mut under_src = false;
    let mut at = PathBuf::new();
    let parts: Vec<_> = path.components().collect();
    for (i, part) in parts.iter().enumerate() {
        at.push(part);
        if under_src && part.as_os_str() == "tests" && i + 1 < parts.len() {
            return Some(at);
        }
        under_src |= part.as_os_str() == "src";
    }
    None
}

/// `path` with `.` and `..` folded away, lexically.
fn folded(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The `#[path = "…"]` value of every `mod` item in `source`, at any depth.
fn path_mods(source: &str) -> Vec<String> {
    struct Paths(Vec<String>);
    impl<'ast> syn::visit::Visit<'ast> for Paths {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            for attr in item.attrs.iter().filter(|a| a.path().is_ident("path")) {
                if let Ok(syn::MetaNameValue {
                    value:
                        syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }),
                    ..
                }) = attr.meta.require_name_value()
                {
                    self.0.push(s.value());
                }
            }
            syn::visit::visit_item_mod(self, item);
        }
    }
    let file = syn::parse_file(source).unwrap_or_else(|e| panic!("not Rust: {e}"));
    let mut found = Paths(Vec::new());
    syn::visit::Visit::visit_file(&mut found, &file);
    found.0
}

/// Does `source` declare, at its top level, a module named `tests` that
/// takes its file from the module tree rather than from a `#[path]`?
fn declares_tests(source: &str) -> bool {
    let file = syn::parse_file(source).unwrap_or_else(|e| panic!("not Rust: {e}"));
    file.items.iter().any(|item| {
        matches!(item, syn::Item::Mod(m)
            if m.ident == "tests" && !m.attrs.iter().any(|a| a.path().is_ident("path")))
    })
}

/// Every `.rs` under a `src/**/tests/` directory below `root/crates`,
/// sorted, with `None` when only a `#[cfg(test)]` module reaches it and
/// `Some(why)` when production code does or no declaration can be read.
/// "Production" is [`production_text`]: the declaration survives it.
fn tests_dir_files(root: &Path) -> Vec<(String, Option<String>)> {
    let mut files = Vec::new();
    every_rs(&root.join("crates"), &mut files);
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    // Judged below the root, so a root whose own path holds a `src` or a
    // `tests` component reads the same as any other.
    let tests_dir = |p: &Path| src_tests_dir(p.strip_prefix(root).ok()?).map(|d| root.join(d));
    let under_src = |p: &Path| {
        p.strip_prefix(root)
            .is_ok_and(|r| r.components().any(|c| c.as_os_str() == "src"))
    };
    let read = |p: &Path| -> (String, String) {
        let raw = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", rel(p)));
        let prod = production_text(&raw)
            .unwrap_or_else(|e| panic!("{} does not parse as Rust: {e}", rel(p)));
        (raw, prod)
    };
    // Every `#[path]` a file outside such a directory declares: where it
    // lands, who declares it, and whether production code does.
    let mut by_path: Vec<(PathBuf, String, bool)> = Vec::new();
    for p in files
        .iter()
        .filter(|p| under_src(p) && tests_dir(p).is_none())
    {
        let (raw, prod) = read(p);
        let in_prod = path_mods(&prod);
        let base = p.parent().unwrap_or(root);
        for value in path_mods(&raw) {
            let survives = in_prod.contains(&value);
            by_path.push((folded(&base.join(&value)), rel(p), survives));
        }
    }
    let mut out: Vec<(String, Option<String>)> = files
        .iter()
        .filter_map(|p| tests_dir(p).map(|dir| (p, dir)))
        .map(|(p, dir)| {
            let here = folded(p);
            let reached: Vec<&(PathBuf, String, bool)> =
                by_path.iter().filter(|(to, _, _)| *to == here).collect();
            let why = if !reached.is_empty() {
                reached
                    .iter()
                    .find(|(_, _, survives)| *survives)
                    .map(|(_, from, _)| {
                        format!("production code reaches it by a #[path] in {from}")
                    })
            } else {
                let parent = dir.parent().unwrap_or(root);
                let candidates: Vec<PathBuf> = [
                    parent.with_extension("rs"),
                    parent.join("mod.rs"),
                    parent.join("lib.rs"),
                    parent.join("main.rs"),
                ]
                .into_iter()
                .filter(|c| c.is_file())
                .collect();
                let parents: Vec<(PathBuf, String, String)> = candidates
                    .into_iter()
                    .map(|c| {
                        let (raw, prod) = read(&c);
                        (c, raw, prod)
                    })
                    .collect();
                if let Some((c, _, _)) = parents.iter().find(|(_, _, prod)| declares_tests(prod)) {
                    Some(format!(
                        "{} declares its `mod tests` without #[cfg(test)]",
                        rel(c)
                    ))
                } else if parents.iter().any(|(_, raw, _)| declares_tests(raw)) {
                    None
                } else {
                    Some(format!(
                        "no `mod tests` declaration this pin can read reaches {} — an orphan, \
                         or a shape it does not follow",
                        rel(&dir)
                    ))
                }
            };
            (rel(p), why)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn every_file_under_a_src_tests_directory_is_reached_only_through_cfg_test() {
    let found = tests_dir_files(&repo_root());
    // Controls: three such files were on main when this landed (the
    // broker-stage door's http/tests/stage.rs and the self-issued
    // rotation's tests/reader.rs and tests/reader/window.rs); a check
    // that found none would pass on any tree.
    assert!(
        found.len() >= 3,
        "the check found under three files in src/**/tests/ — it is not reading the tree: \
         {found:?}"
    );
    let reached: Vec<String> = found
        .iter()
        .filter_map(|(file, why)| why.as_ref().map(|why| format!("{file}: {why}")))
        .collect();
    assert!(
        reached.is_empty(),
        "every_x_boss_user_sender_stamps_the_machine_token skips every directory named \
         `tests`, so a file under src/**/tests/ that production code can reach is a sender it \
         never reads (review of 25fff647, F6). Reach these only through a #[cfg(test)] module, \
         or move them out of the tests directory:\n  {}",
        reached.join("\n  ")
    );
}

#[test]
fn the_tests_directory_check_refuses_a_production_reach_and_accepts_a_test_one() {
    let fixture = boss_testing::scratch_dir("src-tests-dir-reach");
    let write = |rel: &str, body: &str| {
        let path = fixture.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    };
    write(
        "crates/demo/src/lib.rs",
        "#[cfg(test)]\nmod tests;\nmod open;\nmod gated;\n\
         #[path = \"open/tests/leak.rs\"]\nmod leak;\n\
         #[cfg(test)]\n#[path = \"open/tests/fine.rs\"]\nmod fine;\n",
    );
    write("crates/demo/src/tests.rs", "mod a;\n");
    write("crates/demo/src/tests/a.rs", "fn t() {}\n");
    write("crates/demo/src/open.rs", "mod tests;\n");
    write("crates/demo/src/open/tests/b.rs", "fn t() {}\n");
    write("crates/demo/src/open/tests/leak.rs", "fn t() {}\n");
    write("crates/demo/src/open/tests/fine.rs", "fn t() {}\n");
    write(
        "crates/demo/src/gated.rs",
        "#[cfg(test)]\nmod tests {\n    mod c;\n}\n",
    );
    write("crates/demo/src/gated/tests/c.rs", "fn t() {}\n");
    write("crates/demo/src/orphan/tests/d.rs", "fn t() {}\n");
    // A crate's own integration tests are not under src/ and not judged.
    write("crates/demo/tests/e.rs", "fn t() {}\n");
    let verdicts: Vec<(String, bool)> = tests_dir_files(&fixture)
        .into_iter()
        .map(|(file, why)| (file, why.is_none()))
        .collect();
    let expect = |rel: &str, test_only: bool| (format!("crates/demo/src/{rel}"), test_only);
    assert_eq!(
        verdicts,
        vec![
            expect("gated/tests/c.rs", true),
            expect("open/tests/b.rs", false),
            expect("open/tests/fine.rs", true),
            expect("open/tests/leak.rs", false),
            expect("orphan/tests/d.rs", false),
            expect("tests/a.rs", true),
        ]
    );
}

/// The shape this check was asked for, read from the real files: the
/// broker-stage door's `#[path]` into `http/tests/`. Gated it is test
/// code; with its `#[cfg(test)]` dropped it is a production reach, and
/// refused. The real tree cannot be mutated to show this: stage.rs needs
/// test-only imports, so boss-jobs stops compiling before this runs.
#[test]
fn the_broker_stage_doors_path_into_a_tests_directory_is_refused_ungated() {
    let root = repo_root();
    let door = "crates/core/boss-jobs/src/credentials/http.rs";
    let stage = "crates/core/boss-jobs/src/credentials/http/tests/stage.rs";
    let read = |rel: &str| {
        std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    };
    let source = read(door);
    let declared = "#[path = \"http/tests/stage.rs\"]";
    let gated = format!("#[cfg(test)]\n{declared}");
    assert!(
        source.contains(&gated),
        "{door} no longer reaches stage.rs as `{gated}`; move this control to the shape it has"
    );
    let fixture = boss_testing::scratch_dir("src-tests-dir-real-path");
    let write = |rel: &str, body: &str| {
        let path = fixture.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    };
    write(stage, &read(stage));
    write(door, &source);
    assert_eq!(tests_dir_files(&fixture), vec![(stage.to_string(), None)]);
    write(door, &source.replacen(&gated, declared, 1));
    let found = tests_dir_files(&fixture);
    assert!(
        matches!(found.as_slice(), [(file, Some(why))] if file == stage && why.contains(door)),
        "{found:?}"
    );
}

#[test]
fn inbound_runner_resolution_is_not_an_outgoing_sender() {
    let root = repo_root();
    let path = "crates/core/boss-jobs/src/runner_credential.rs";
    assert!(
        !senders(&root).iter().any(|(file, _, _)| file == path),
        "the credential resolver rewrites an incoming axum request; it sends no HTTP request"
    );
    // An inbound classification must not hide an outgoing client later
    // added to the same middleware file. Keep the actual inbound source.
    let fixture = boss_testing::scratch_dir("runner-resolution-outgoing-control");
    let target = fixture.join(path);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let mut source = std::fs::read_to_string(root.join(path)).unwrap();
    source.push_str(
        "\nasync fn outgoing_control() {\n let client = reqwest::Client::new();\n client.get(\"http://example.invalid\").header(\"x-boss-user\", \"asserted\").send().await;\n}\n",
    );
    std::fs::write(&target, source).unwrap();
    assert!(
        senders(&fixture)
            .iter()
            .any(|(file, lines, stamped)| file == path && !lines.is_empty() && !stamped),
        "an unstamped outgoing sender in the resolver must still be refused"
    );
}
