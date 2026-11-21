//! Every service on the port roster answers its health read with the
//! build commit (backlog 56f88ada, 2026-11-20).
//!
//! WHAT WAS MEASURED. `/api/<svc>/health` is how a reader learns which
//! build answered — `boss running`, the conductor's convergence check
//! and the yard's tower all read `capabilities.commit` off it. Swept on
//! 2026-11-19 (run bfc2e71c) and again on 2026-11-20 at origin/main
//! 0deae9d0 (run 06633389): fifteen services built the body through
//! `boss_core::startup::health_response` / `Capabilities::new`, which
//! fill `commit` from BOSS_BUILD_COMMIT, and ten built it by hand with
//! `json!` and no commit — classes, locations, subject-kinds, events,
//! search, views, campaigns, customers, products and accounts (an
//! inline closure). Every service binary is the one image, so the
//! commit was available to all of them; the hand-built ones simply
//! never asked. A read without its version is a guess, and an estate
//! with ten such reads cannot be checked for a stale image.
//!
//! THE RULE. For every row of `boss_ports::all()` that serves a health
//! body, some production `.route("<its health path>", get(<handler>))`
//! exists under `crates/`, and EVERY such route's handler builds its
//! body through `health_response(…)` or `Capabilities::new(…)` — the one
//! place the commit is read, so it is never copied per service.
//!
//! WHAT IT DOES NOT HOLD, said so it is not assumed. It reads syntax,
//! not a served response: building every service's router here would
//! need every service's state (a pool, a bus, a policy client) for one
//! JSON field. So a handler that builds `Capabilities` and then drops
//! it from its body passes; the call is the evidence, not the bytes.
//! `boss_core::startup`'s own tests hold that `Capabilities::new`
//! carries `build_commit()`. A handler is looked up by name in its
//! route's file, then in its crate's `src/`, and every function of that
//! name there must stamp — a name that is ambiguous fails loudly rather
//! than passing on the wrong function.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

/// Roster rows with no health BODY to stamp, each with why. A row named
/// here that leaves the roster fails the pin, so the list cannot rot.
const NO_HEALTH_BODY: [(&str, &str); 2] = [
    (
        "gateway",
        "its /health is the edge's constant `ok` liveness answer, pinned by \
         the_liveness_answer_is_still_ok_to_a_sessionless_caller; it fronts \
         the others and serves no /api/gateway/health",
    ),
    (
        "sim-control",
        "the sim daemon's localhost-only control server, not gateway-proxied \
         and not an /api/<svc>/health surface",
    ),
];

/// Roster rows whose router spells the health route otherwise than
/// `/api/<name>/health`: boss-simulator nests its api under
/// `/simulator`, so the route literal is `/api/health` and the wire
/// path `/simulator/api/health`.
const ROUTE_SPELLED_OTHERWISE: [(&str, &str); 1] = [("simulator", "/api/health")];

fn health_path(service: &str) -> String {
    ROUTE_SPELLED_OTHERWISE
        .iter()
        .find(|(name, _)| *name == service)
        .map_or_else(|| format!("/api/{service}/health"), |(_, p)| p.to_string())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules" || n == "tests")
            {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("tests.rs") {
            out.push(p);
        }
    }
}

/// An item under `#[cfg(test)]`, or a `#[test]`/`#[tokio::test]` fn:
/// a router a test builds to prove something is not the service's.
fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let p = a.path();
        if p.is_ident("cfg") {
            return a.meta.require_list().is_ok_and(|l| {
                l.tokens
                    .to_string()
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|w| w == "test")
            });
        }
        p.segments.last().is_some_and(|s| s.ident == "test")
    })
}

fn path_names(e: &syn::Expr) -> Vec<String> {
    match e {
        syn::Expr::Path(p) => p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Does this code call `…health_response(…)` or `…Capabilities::new(…)`?
#[derive(Default)]
struct Stamps(bool);

impl<'a> Visit<'a> for Stamps {
    fn visit_expr_call(&mut self, c: &'a syn::ExprCall) {
        let names = path_names(&c.func);
        if names.last().is_some_and(|n| n == "health_response")
            || names.ends_with(&["Capabilities".to_string(), "new".to_string()])
        {
            self.0 = true;
        }
        syn::visit::visit_expr_call(self, c);
    }
    /// `serde_json::json!(health_response(…))` hides the call in tokens.
    fn visit_macro(&mut self, m: &'a syn::Macro) {
        type Args = syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>;
        if let Ok(args) = m.parse_body_with(Args::parse_terminated) {
            for e in &args {
                <Self as Visit<'_>>::visit_expr(self, e);
            }
        }
        syn::visit::visit_macro(self, m);
    }
}

fn stamps_block(b: &syn::Block) -> bool {
    let mut s = Stamps::default();
    s.visit_block(b);
    s.0
}

/// The method router's `get(<handler>)`, through any `.post(…)` chained
/// after it.
fn get_handler(e: &syn::Expr) -> Option<&syn::Expr> {
    match e {
        syn::Expr::Call(c) if path_names(&c.func).last().is_some_and(|n| n == "get") => {
            c.args.first()
        }
        syn::Expr::MethodCall(m) => get_handler(&m.receiver),
        _ => None,
    }
}

enum Handler {
    /// A function by name (turbofish dropped).
    Named(String),
    /// A closure or other inline expression, already judged.
    Inline(bool),
}

struct Route {
    path: String,
    line: usize,
    handler: Option<Handler>,
}

/// One production file: its health routes and its functions.
#[derive(Default)]
struct Scan {
    wanted: Vec<String>,
    routes: Vec<Route>,
    fns: Vec<(String, bool)>,
}

impl<'a> Visit<'a> for Scan {
    fn visit_item_mod(&mut self, m: &'a syn::ItemMod) {
        if !is_test(&m.attrs) {
            syn::visit::visit_item_mod(self, m);
        }
    }
    fn visit_item_fn(&mut self, f: &'a syn::ItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        self.fns
            .push((f.sig.ident.to_string(), stamps_block(&f.block)));
        syn::visit::visit_item_fn(self, f);
    }
    fn visit_impl_item_fn(&mut self, f: &'a syn::ImplItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        self.fns
            .push((f.sig.ident.to_string(), stamps_block(&f.block)));
        syn::visit::visit_impl_item_fn(self, f);
    }
    fn visit_expr_method_call(&mut self, m: &'a syn::ExprMethodCall) {
        if m.method == "route"
            && let Some(syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            })) = m.args.first()
            && self.wanted.contains(&s.value())
        {
            let handler = m.args.get(1).and_then(get_handler).map(|h| match h {
                syn::Expr::Path(_) => {
                    Handler::Named(path_names(h).last().cloned().unwrap_or_default())
                }
                other => {
                    let mut st = Stamps::default();
                    st.visit_expr(other);
                    Handler::Inline(st.0)
                }
            });
            self.routes.push(Route {
                path: s.value(),
                line: s.span().start().line,
                handler,
            });
        }
        syn::visit::visit_expr_method_call(self, m);
    }
}

/// `crates/<tier>/<crate>` for a file under that crate's `src/`.
fn crate_dir(rel: &Path) -> PathBuf {
    rel.components().take(3).collect()
}

#[test]
fn every_roster_service_builds_its_health_body_with_the_build_commit() {
    let root = repo_root();
    let roster: Vec<&str> = boss_ports::all().map(|s| s.name).collect();

    let mut failures = Vec::new();
    for (name, why) in NO_HEALTH_BODY.iter().chain(ROUTE_SPELLED_OTHERWISE.iter()) {
        if !roster.contains(name) {
            failures.push(format!(
                "{name}: named by this pin ({why}) but no longer a boss-ports row — drop it here"
            ));
        }
    }
    let served: Vec<&str> = roster
        .iter()
        .copied()
        .filter(|n| !NO_HEALTH_BODY.iter().any(|(x, _)| x == n))
        .collect();
    let wanted: Vec<String> = served.iter().map(|n| health_path(n)).collect();

    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    files.sort();
    let read = |f: &Path| {
        std::fs::read_to_string(f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()))
    };
    let rel = |f: &Path| f.strip_prefix(&root).unwrap_or(f).to_path_buf();
    // Parse only the crates that spell a health path: a handler lives in
    // its route's crate, and the rest of the tree is not this pin's.
    let crates: Vec<PathBuf> = files
        .iter()
        .filter(|f| {
            let src = read(f);
            wanted.iter().any(|w| src.contains(&format!("\"{w}\"")))
        })
        .map(|f| crate_dir(&rel(f)))
        .collect();
    let mut scans: Vec<(PathBuf, Scan)> = Vec::new();
    for f in files
        .iter()
        .filter(|f| crates.iter().any(|c| rel(f).starts_with(c)))
    {
        let src = read(f);
        let parsed = syn::parse_file(&src).unwrap_or_else(|e| panic!("parse {}: {e}", f.display()));
        let mut scan = Scan {
            wanted: wanted.clone(),
            ..Scan::default()
        };
        scan.visit_file(&parsed);
        scans.push((rel(f), scan));
    }

    for service in &served {
        let path = health_path(service);
        let mut found = 0;
        for (rel, scan) in &scans {
            for route in scan.routes.iter().filter(|r| r.path == path) {
                found += 1;
                let at = format!("{service}: {path} at {}:{}", rel.display(), route.line);
                match &route.handler {
                    None => failures.push(format!("{at} is not routed through get(<handler>)")),
                    Some(Handler::Inline(true)) => {}
                    Some(Handler::Inline(false)) => failures.push(format!(
                        "{at} answers from an inline handler that never calls health_response / Capabilities::new, so its body carries no commit"
                    )),
                    Some(Handler::Named(h)) => {
                        let same_file: Vec<bool> =
                            scan.fns.iter().filter(|(n, _)| n == h).map(|(_, s)| *s).collect();
                        let candidates = if same_file.is_empty() {
                            let krate = crate_dir(rel);
                            scans
                                .iter()
                                .filter(|(r, _)| r.starts_with(&krate))
                                .flat_map(|(_, s)| s.fns.iter())
                                .filter(|(n, _)| n == h)
                                .map(|(_, s)| *s)
                                .collect()
                        } else {
                            same_file
                        };
                        if candidates.is_empty() {
                            failures.push(format!("{at} routes to `{h}`, which this pin cannot find in its crate"));
                        } else if !candidates.iter().all(|s| *s) {
                            failures.push(format!(
                                "{at} routes to `{h}`, which builds its body without health_response / Capabilities::new, so it carries no commit"
                            ));
                        }
                    }
                }
            }
        }
        if found == 0 {
            failures.push(format!(
                "{service}: no production route for {path} under crates/ — a roster row with no health body belongs in NO_HEALTH_BODY with its reason"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} health surface(s) cannot say which build answered (backlog 56f88ada):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
