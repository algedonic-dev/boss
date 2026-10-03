//! Every port with an in-memory AND a Postgres adapter runs one
//! `boss_testing::adapters_agree!` suite naming both, or is on
//! [`EXEMPT`] with its reason (backlog be459ab9, design 3036296f
//! mechanism C, David approved 2026-09-27).
//!
//! WHY A TEST. Twenty-seven cars suited the ports one at a time, from a
//! census kept in the packet's metadata — and that census drifted twice
//! while they ran: measured 2026-09-29 by a prefix scan keyed on the
//! crate, it could not see the policy store (its in-memory adapter lives
//! in `boss-policy-client`, its Postgres one in `boss-policy`), and by
//! 2026-10-01 its `remaining_core` list was stale by a dozen landed
//! suites. A census remembered in a packet drifts; a test does not
//! (CLAUDE.md §9a). This is the census, measured on every gate, and the
//! next port that grows a second adapter fails here until it is suited
//! or exempted — rather than waiting for someone to re-run a scan.
//!
//! WHAT IS A PORT WITH TWO ADAPTERS. A trait declared under
//! `crates/*/*/src/` that is implemented, anywhere under those trees
//! (`#[cfg(test)]` code included — a test double is still the second
//! statement of the contract), by a type named `InMemory*` AND by one
//! named `Pg*` or `Postgres*`. Traits are matched by NAME across the
//! whole tree, not per crate, which is what makes the policy store
//! visible: [`CONTROLS`] holds the scan to seeing it. `tests/`
//! directories are not read for impls: the suites' own fixture traits
//! (`World for InMemory`, the moves suite's `Logged`) are scaffolding, not ports.
//!
//! WHAT IS SUITED. A file anywhere under `crates/` — or, inside one, a
//! `mod` body — that invokes `adapters_agree!` itself and whose CODE
//! (comments and string literals are not tokens that count) names the
//! TRAIT, one of the port's in-memory types AND one of its Postgres
//! types. The trait is required because two adapter types implement
//! every trait they implement: without it, a `JobsPurge` added to the
//! jobs store's two adapters read as suited by the list-filter suite,
//! whose cases never call it (review 0880db62). Naming them is the
//! measurable half of "runs both"; whether the cases are any good is the
//! suite's own review, not this pin's. A binary crate's suite lives in
//! `src/`, beside other tests, so it sits in a module of its own and
//! only that module is read.
//!
//! WHAT THIS CANNOT SEE, said so nobody reads more into a green than it
//! holds:
//! - a port whose adapters carry other names (an `HttpStamps` beside a
//!   Postgres read is not two of these);
//! - a suite that names the trait and both types without running a case
//!   of that trait against one of them — naming is not calling;
//! - two traits of one name in different crates: they are one row here
//!   (`CalendarClient` and `Upstream` are each declared twice today),
//!   so a suite of one counts for the other;
//! - an impl on a wrapper or a reference (`for Arc<InMemoryX>`,
//!   `for &PostgresX`), and an impl a macro generates — only a written `impl
//!   Trait for Name` is read.
//!
//! tree-wide pin — it scans every crate's `src/` and every suite file,
//! so no changed-file map can attribute it to this crate, and every
//! scoped gate runs it whatever its scope (`tree_wide_pins` in
//! infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::visit::Visit;

/// Ports with both adapters that run no suite, each with the measured
/// reason. An entry that is no longer a two-adapter port, or that is
/// now suited, fails the pin: a stale exemption is a hole shaped like a
/// port that no longer needs one.
const EXEMPT: [(&str, &str); 2] = [
    (
        "SubjectExistenceCheck",
        "a read-only existence probe with no state to disagree on: the in-memory type is a \
         #[cfg(test)] double whose unseeded kinds pass by design, production's in-memory path \
         wires None (boss-jobs http/mod.rs), and its Postgres adapter runs its own suite against \
         the subjects table (boss-jobs tests/subject_existence_pg.rs)",
    ),
    (
        "TenantPublishes",
        "needs a design over MAX_STAGED_EVENT_BYTES: the port has only `record`, so a suite \
         would need a reader per adapter to judge anything, and its agreeing cases would not \
         pin the one known disagreement — Postgres refuses an event over the bound while the \
         in-memory adapter accepts it, and the constant compiles only under boss-events' \
         postgres feature",
    ),
];

/// Ports the scan must find, suited — the control read whose answer is
/// already known. The jobs store was the first suite; the policy store
/// is the one a per-crate scan cannot see (its adapters live in two
/// crates).
const CONTROLS: [&str; 2] = ["JobsRepository", "PolicyRepository"];

fn is_in_memory(ty: &str) -> bool {
    ty.starts_with("InMemory")
}

fn is_postgres(ty: &str) -> bool {
    ty.starts_with("Postgres")
        || ty
            .strip_prefix("Pg")
            .and_then(|rest| rest.chars().next())
            .is_some_and(|c| c.is_ascii_uppercase())
}

/// What one pass over `src/` found.
#[derive(Default)]
struct Impls {
    /// Every trait declared outside test code, by name. A trait a
    /// `#[cfg(test)]` module declares to seed its own suite is
    /// scaffolding, not a port.
    declared: BTreeSet<String>,
    /// trait name → (implementing type, repo-relative file).
    by_trait: BTreeMap<String, BTreeSet<(String, String)>>,
    file: String,
    /// How many `#[cfg(test)]` modules enclose the item being visited.
    in_test: usize,
}

fn last_ident(path: &syn::Path) -> Option<String> {
    path.segments.last().map(|s| s.ident.to_string())
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && matches!(&a.meta, syn::Meta::List(l)
                if l.tokens.clone().into_iter().any(|t| matches!(t, TokenTree::Ident(i) if i == "test")))
    })
}

impl<'ast> Visit<'ast> for Impls {
    fn visit_item_mod(&mut self, i: &'ast syn::ItemMod) {
        let test = is_cfg_test(&i.attrs);
        self.in_test += usize::from(test);
        syn::visit::visit_item_mod(self, i);
        self.in_test -= usize::from(test);
    }

    fn visit_item_trait(&mut self, i: &'ast syn::ItemTrait) {
        if self.in_test == 0 && !is_cfg_test(&i.attrs) {
            self.declared.insert(i.ident.to_string());
        }
        syn::visit::visit_item_trait(self, i);
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if let (Some((_, trait_path, _)), syn::Type::Path(ty)) = (&i.trait_, &*i.self_ty)
            && let (Some(tr), Some(ty)) = (last_ident(trait_path), last_ident(&ty.path))
        {
            self.by_trait
                .entry(tr)
                .or_default()
                .insert((ty, self.file.clone()));
        }
        syn::visit::visit_item_impl(self, i);
    }
}

/// `.rs` files under `dir`; `tests/` directories skipped when asked.
fn rust_files(dir: &Path, skip_tests: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "target" && !(skip_tests && (name == "tests" || name == "benches")) {
                rust_files(&path, skip_tests, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Every crate directory, `crates/<tier>/<name>`.
fn crate_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))
        .expect("crates/ is readable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .flat_map(|tier| {
            std::fs::read_dir(tier)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect::<Vec<_>>()
        })
        .collect();
    out.sort();
    out
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Every identifier in a token stream, through every group.
fn idents(tokens: TokenStream, out: &mut BTreeSet<String>) {
    for t in tokens {
        match t {
            TokenTree::Ident(i) => {
                out.insert(i.to_string());
            }
            TokenTree::Group(g) => idents(g.stream(), out),
            _ => {}
        }
    }
}

/// Does this level of the stream itself invoke `adapters_agree!` (bare
/// or by path)? Groups below it are not read: an invocation belongs to
/// the module whose body holds it.
fn invokes_the_suite(tokens: TokenStream) -> bool {
    let mut prev_is_suite = false;
    for t in tokens {
        match t {
            TokenTree::Punct(p) if p.as_char() == '!' && prev_is_suite => return true,
            TokenTree::Ident(i) => prev_is_suite = i == "adapters_agree",
            _ => prev_is_suite = false,
        }
    }
    false
}

/// The scopes that invoke the suite: the file itself, or any `mod`
/// body at any depth. The scope, not the file, is what must name both
/// adapters — a source file under `src/` (a binary's suite cannot live
/// in `tests/`) carries other tests that name other adapters, and
/// boss-cli's tenant_stamp.rs names the jobs API's
/// `InMemoryTenantPublishes` in a door test beside its own suite.
fn suite_scopes(tokens: TokenStream, out: &mut Vec<TokenStream>) {
    if invokes_the_suite(tokens.clone()) {
        out.push(tokens.clone());
    }
    let level: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in level.iter().enumerate() {
        if let (TokenTree::Ident(kw), Some(TokenTree::Ident(_)), Some(TokenTree::Group(body))) =
            (t, level.get(i + 1), level.get(i + 2))
            && kw == "mod"
            && body.delimiter() == proc_macro2::Delimiter::Brace
        {
            suite_scopes(body.stream(), out);
        }
    }
}

/// The census: each two-adapter port, its adapters, and the suite
/// files that name both.
struct Port {
    in_memory: BTreeSet<(String, String)>,
    postgres: BTreeSet<(String, String)>,
    suites: Vec<String>,
}

fn census(root: &Path) -> BTreeMap<String, Port> {
    let mut impls = Impls::default();
    let mut unparsable = Vec::new();
    let mut src_files = 0;
    // Each suite file and the identifiers its code names.
    let mut suites: Vec<(String, BTreeSet<String>)> = Vec::new();
    for krate in crate_dirs(root) {
        let mut paths = Vec::new();
        rust_files(&krate.join("src"), true, &mut paths);
        for path in &paths {
            src_files += 1;
            let source = std::fs::read_to_string(path).expect("a source file is readable");
            impls.file = rel(root, path);
            match syn::parse_file(&source) {
                Ok(file) => impls.visit_file(&file),
                Err(e) => unparsable.push(format!("{}: {e}", impls.file)),
            }
        }
        let mut all = Vec::new();
        rust_files(&krate, false, &mut all);
        for path in all {
            let source = std::fs::read_to_string(&path).expect("a source file is readable");
            if !source.contains("adapters_agree") {
                continue;
            }
            let Ok(tokens) = source.parse::<TokenStream>() else {
                unparsable.push(rel(root, &path));
                continue;
            };
            let mut scopes = Vec::new();
            suite_scopes(tokens, &mut scopes);
            for scope in scopes {
                let mut names = BTreeSet::new();
                idents(scope, &mut names);
                suites.push((rel(root, &path), names));
            }
        }
    }
    assert!(
        unparsable.is_empty(),
        "files the census cannot read (a file it cannot parse is a port it cannot see):\n{}",
        unparsable.join("\n")
    );
    assert!(
        src_files > 500 && suites.len() > 30,
        "the census read {src_files} source files and {} suite files — a scan this small \
         is a wrong root, not a clean tree",
        suites.len()
    );
    impls
        .by_trait
        .iter()
        .filter(|(tr, _)| impls.declared.contains(*tr))
        .filter_map(|(tr, types)| {
            let in_memory: BTreeSet<_> = types
                .iter()
                .filter(|(t, _)| is_in_memory(t))
                .cloned()
                .collect();
            let postgres: BTreeSet<_> = types
                .iter()
                .filter(|(t, _)| is_postgres(t))
                .cloned()
                .collect();
            if in_memory.is_empty() || postgres.is_empty() {
                return None;
            }
            let suites = suites
                .iter()
                .filter(|(_, names)| suites_the_port(tr, names, &in_memory, &postgres))
                .map(|(f, _)| f.clone())
                .collect();
            Some((
                tr.clone(),
                Port {
                    in_memory,
                    postgres,
                    suites,
                },
            ))
        })
        .collect()
}

/// Does a suite scope, by the identifiers its code names, suite the
/// port `tr`? It must name the trait as well as one adapter of each
/// kind: two adapter types implement every trait they implement, and a
/// suite whose cases are bounded by one of them says nothing about the
/// others (review 0880db62).
fn suites_the_port(
    tr: &str,
    names: &BTreeSet<String>,
    in_memory: &BTreeSet<(String, String)>,
    postgres: &BTreeSet<(String, String)>,
) -> bool {
    let names_one = |of: &BTreeSet<(String, String)>| of.iter().any(|(t, _)| names.contains(t));
    names.contains(tr) && names_one(in_memory) && names_one(postgres)
}

fn adapters(of: &BTreeSet<(String, String)>) -> String {
    of.iter()
        .map(|(t, f)| format!("{t} ({f})"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn every_port_with_an_in_memory_and_a_postgres_adapter_runs_one_suite_or_says_why_not() {
    let root = repo_root();
    let ports = census(&root);
    let exempt: BTreeMap<&str, &str> = EXEMPT.into_iter().collect();

    // The census, printed whole, so a red or a green can be read
    // without re-running the scan by hand.
    for (tr, p) in &ports {
        let state = match (p.suites.first(), exempt.get(tr.as_str())) {
            (Some(f), _) => format!("suited by {f}"),
            (None, Some(_)) => "exempt".to_string(),
            (None, None) => "UNSUITED".to_string(),
        };
        eprintln!(
            "{tr}: {state} — {} | {}",
            adapters(&p.in_memory),
            adapters(&p.postgres)
        );
    }

    let mut wrong = Vec::new();
    for c in CONTROLS {
        match ports.get(c) {
            Some(p) if !p.suites.is_empty() => {}
            Some(_) => wrong.push(format!("control {c} is a port but reads unsuited")),
            None => wrong.push(format!(
                "control {c} is not seen as a two-adapter port — the scan is blind, not the tree clean"
            )),
        }
    }
    for (tr, p) in &ports {
        if p.suites.is_empty() && !exempt.contains_key(tr.as_str()) {
            wrong.push(format!(
                "{tr} has an in-memory adapter ({}) and a Postgres one ({}) and no \
                 boss_testing::adapters_agree! file names both: write its suite, or put it on \
                 EXEMPT in this file with the measured reason",
                adapters(&p.in_memory),
                adapters(&p.postgres)
            ));
        }
    }
    for (tr, why) in EXEMPT {
        match ports.get(tr) {
            None => wrong.push(format!(
                "EXEMPT names {tr}, which is no longer a port with both adapters: delete the entry"
            )),
            Some(p) if !p.suites.is_empty() => wrong.push(format!(
                "EXEMPT names {tr}, which is suited by {}: delete the entry",
                p.suites.join(", ")
            )),
            Some(_) if why.trim().is_empty() => {
                wrong.push(format!("EXEMPT names {tr} with no reason"))
            }
            Some(_) => {}
        }
    }
    assert!(
        wrong.is_empty(),
        "{} port(s) out of the census:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn the_prefixes_read_the_adapters_and_not_their_neighbours() {
    assert!(is_in_memory("InMemoryJobs"));
    // Spelled in two halves: a whole adapter name in this file would read
    // to a-pg-test-declares-its-feature as a test reaching a Postgres
    // adapter, and this pin reaches none.
    assert!(is_postgres(&format!("{}{}", "Pg", "Jobs")));
    assert!(is_postgres("PostgresEventStore"));
    // `Pg` is a prefix only before a capital: a `Page` is not an adapter.
    assert!(!is_postgres("Page"));
    assert!(!is_postgres("Pg"));
    assert!(!is_in_memory("HttpStamps"));
}

#[test]
fn a_suite_is_read_from_the_invocation_not_from_a_mention() {
    let named: TokenStream =
        "boss_testing::adapters_agree! { adapters { a => (A, ()) } cases { c } }"
            .parse()
            .unwrap();
    assert!(invokes_the_suite(named));
    // A comment or a string naming the macro is not a suite.
    let mention: TokenStream = "// adapters_agree! here\nconst S: &str = \"adapters_agree!\";"
        .parse()
        .unwrap();
    assert!(!invokes_the_suite(mention));
}

/// Review 0880db62 (2026-10-01): a suite's two adapter types implement
/// every trait they implement, and a suite of one of them must not
/// count for the others. Added a `JobsPurge` on `InMemoryJobs` and the
/// Postgres jobs store, the pin called it suited by the list-filter
/// suite, whose cases never call it.
#[test]
fn a_suite_counts_only_for_the_trait_its_scope_names() {
    let adapter = |t: &str| BTreeSet::from([(t.to_string(), "src/x.rs".to_string())]);
    let (in_memory, postgres) = (adapter("InMemoryJobs"), adapter("PostgresJobs"));
    let suite: BTreeSet<String> = ["JobsRepository", "InMemoryJobs", "PostgresJobs"]
        .map(String::from)
        .into();
    assert!(suites_the_port(
        "JobsRepository",
        &suite,
        &in_memory,
        &postgres
    ));
    assert!(
        !suites_the_port("JobsPurge", &suite, &in_memory, &postgres),
        "a suite naming both adapters but not the trait suites another trait"
    );
}

#[test]
fn a_suite_in_a_module_names_only_what_its_module_names() {
    let file: TokenStream = "use a::InMemoryOther; fn door() { PostgresOther::new(); } \
         mod tests { mod agree { adapters_agree! { adapters { m => (InMemoryX, ()) } cases { c } } \
         fn pg() -> PostgresX { todo!() } } }"
        .parse()
        .unwrap();
    let mut scopes = Vec::new();
    suite_scopes(file, &mut scopes);
    assert_eq!(scopes.len(), 1, "one scope: the module that invokes it");
    let mut names = BTreeSet::new();
    idents(scopes.remove(0), &mut names);
    assert!(names.contains("InMemoryX") && names.contains("PostgresX"));
    assert!(!names.contains("InMemoryOther") && !names.contains("PostgresOther"));
}
