//! A test that re-runs its own binary as a child never judges the child
//! with a wall clock that started before the child did (backlog
//! 7e30ddb0).
//!
//! WHAT WAS MEASURED. Two gate reds on 2026-10-19, each in a file the
//! gated car did not touch, and neither reproducing in ten runs alone:
//! gate 99ece855 on boss-cli's
//! `prove::tests::root_opens_nothing_in_the_view_before_the_account_answers`
//! and gate 5fc86d87 on
//! `train::diagnostic_adapter::tests::diagnostic_fetch_uses_published_auth_and_keeps_refusal_originals`.
//! Both re-run the WHOLE debug test binary (`current_exe() --exact
//! <test>`) and give the child a fixed 30 s from before it was spawned —
//! so the window holds the exec of a whole debug binary, its page-in and
//! libtest's start, which a gate pod running the full suite beside two
//! other gates does not serve in 30 s, as well as the work the bound is
//! there to judge. The work is what can hang; the start is the host.
//!
//! THE RULE. A function that re-execs this test binary — a call of
//! `current_exe` beside the `"--exact"` that names the test — constructs
//! no wall-clock bound: no `Instant::now() + …` or
//! `Instant::now().checked_add(…)`, no `….elapsed()` compared with `<`,
//! `<=`, `>` or `>=`, no `recv_timeout`, no `timeout(…)`. It goes
//! through `boss_testing::reexec` instead, whose child says
//! `reexec::ready()` once it is running and whose clock starts there,
//! so the test still catches the hang it was written for and no longer
//! catches a slow host.
//!
//! WHAT IT DOES NOT HOLD, said so it is not assumed. It reads one
//! function at a time (closures inside it included), so a bound built
//! in a DIFFERENT function from the spawn — a `finish` helper beside a
//! `spawn` one — is not seen; the re-execs of that shape on 2026-10-21
//! (`gate/reservation/native_tests.rs`) were moved to the helper by
//! hand. It reads syntax, not types, so `let now = Instant::now();`
//! followed by `now + d` is not seen: the `+` is on a variable the
//! detector cannot know is an Instant (review 12a4ee82, N1, which also
//! named the `elapsed()` comparison and `checked_add`, both held now).
//! A re-exec through `boss_testing::reexec` is not a `current_exe`
//! call, so a bound its caller builds after `ready()` is the caller's
//! to place correctly. No crate needs an inline copy of the helper's
//! wait: boss-core, which boss-testing depends on, takes it as a
//! dev-dependency, since its inline copy dropped the start ceiling to
//! get past this pin and hung on a wedge (review 12a4ee82, B1).
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// The one file allowed both halves: the helper, whose bound is the
/// work bound and starts at the child's `ready()`.
const EXEMPT: &[&str] = &["crates/core/boss-testing/src/reexec.rs"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
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
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Does `e` name a path ending in `last` (`std::env::current_exe`,
/// `tokio::time::timeout`, a bare `timeout`)?
fn path_ends(e: &syn::Expr, last: &str) -> bool {
    matches!(e, syn::Expr::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == last))
}

/// Is `e` a call of `…Instant::now()`?
fn is_instant_now(e: &syn::Expr) -> bool {
    let syn::Expr::Call(c) = e else {
        return false;
    };
    let syn::Expr::Path(p) = &*c.func else {
        return false;
    };
    let names: Vec<String> = p
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    names.ends_with(&["Instant".to_string(), "now".to_string()])
}

/// Is `e` a call of `….elapsed()` — a clock read against a start?
fn is_elapsed(e: &syn::Expr) -> bool {
    matches!(e, syn::Expr::MethodCall(m) if m.method == "elapsed")
}

/// One function body: where it calls `current_exe`, whether it names
/// `"--exact"`, and the lines that build a wall-clock bound.
#[derive(Default)]
struct Body {
    reexec: Vec<usize>,
    exact: bool,
    bounds: Vec<usize>,
}

impl<'a> Visit<'a> for Body {
    fn visit_expr_call(&mut self, c: &'a syn::ExprCall) {
        if path_ends(&c.func, "current_exe") {
            self.reexec.push(c.span().start().line);
        }
        if path_ends(&c.func, "timeout") {
            self.bounds.push(c.span().start().line);
        }
        syn::visit::visit_expr_call(self, c);
    }
    fn visit_expr_method_call(&mut self, m: &'a syn::ExprMethodCall) {
        // `Instant::now().checked_add(d)` is the same deadline as `+`.
        if m.method == "recv_timeout" || (m.method == "checked_add" && is_instant_now(&m.receiver))
        {
            self.bounds.push(m.span().start().line);
        }
        syn::visit::visit_expr_method_call(self, m);
    }
    fn visit_expr_binary(&mut self, b: &'a syn::ExprBinary) {
        let deadline = matches!(b.op, syn::BinOp::Add(_)) && is_instant_now(&b.left);
        // `t.elapsed() < d` — a clock started at `t` and compared to a
        // bound — is the commonest way to write the same thing, and the
        // shape reexec.rs uses itself (review 12a4ee82, N1).
        let compared = matches!(
            b.op,
            syn::BinOp::Lt(_) | syn::BinOp::Le(_) | syn::BinOp::Gt(_) | syn::BinOp::Ge(_)
        ) && (is_elapsed(&b.left) || is_elapsed(&b.right));
        if deadline || compared {
            self.bounds.push(b.span().start().line);
        }
        syn::visit::visit_expr_binary(self, b);
    }
    /// A macro's body is tokens to syn, so `assert!(t.elapsed() < d)`
    /// would hide its clock. Read a body that parses as comma-separated
    /// expressions (assert!, panic!, format! and their kin) as those.
    fn visit_macro(&mut self, m: &'a syn::Macro) {
        type Args = syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>;
        if let Ok(args) = m.parse_body_with(Args::parse_terminated) {
            for e in &args {
                // The parsed body lives here, not for 'a: visit it under
                // its own lifetime.
                <Self as Visit<'_>>::visit_expr(self, e);
            }
        }
        syn::visit::visit_macro(self, m);
    }
    fn visit_lit_str(&mut self, s: &'a syn::LitStr) {
        if s.value() == "--exact" {
            self.exact = true;
        }
    }
}

/// Every function in a file, by name, with its body's findings.
struct Fns(Vec<(String, Body)>);

impl Fns {
    fn take(&mut self, name: String, block: &syn::Block) {
        let mut body = Body::default();
        body.visit_block(block);
        self.0.push((name, body));
    }
}

impl<'a> Visit<'a> for Fns {
    fn visit_item_fn(&mut self, f: &'a syn::ItemFn) {
        self.take(f.sig.ident.to_string(), &f.block);
        syn::visit::visit_item_fn(self, f);
    }
    fn visit_impl_item_fn(&mut self, f: &'a syn::ImplItemFn) {
        self.take(f.sig.ident.to_string(), &f.block);
        syn::visit::visit_impl_item_fn(self, f);
    }
}

/// `fn <name>: re-exec at <lines>, bound at <lines>` for each function
/// in `src` that re-execs its test binary and builds a clock of its own.
fn bare_bounds(src: &str) -> Result<Vec<String>, syn::Error> {
    let file = syn::parse_file(src)?;
    let mut fns = Fns(Vec::new());
    fns.visit_file(&file);
    Ok(fns
        .0
        .into_iter()
        .filter(|(_, b)| !b.reexec.is_empty() && b.exact && !b.bounds.is_empty())
        .map(|(name, b)| {
            format!(
                "fn {name}: re-exec at {:?}, bound at {:?}",
                b.reexec, b.bounds
            )
        })
        .collect())
}

fn scan(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let mut found = Vec::new();
    for p in &files {
        let Some(rel) = p
            .strip_prefix(root)
            .ok()
            .map(|r| r.to_string_lossy().replace('\\', "/"))
        else {
            continue;
        };
        if EXEMPT.contains(&rel.as_str()) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(p) else {
            continue;
        };
        // Cheap first: most files name neither.
        if !src.contains("current_exe") {
            continue;
        }
        let hits =
            bare_bounds(&src).unwrap_or_else(|e| panic!("{rel} does not parse as Rust: {e}"));
        found.extend(hits.into_iter().map(|h| format!("{rel} {h}")));
    }
    found.sort();
    found
}

#[test]
fn no_re_exec_judges_its_child_by_a_clock_started_before_it() {
    let found = scan(&repo_root());
    assert!(
        found.is_empty(),
        "these functions re-run their test binary and judge the child by a wall clock of \
         their own, which starts before the child's exec and page-in and so reddens a gate \
         on correct code under load (backlog 7e30ddb0). Spawn the child through \
         `boss_testing::reexec::ReExec`, have its inner branch call \
         `boss_testing::reexec::ready()` first, and bound only the work after that:\n  {}",
        found.join("\n  ")
    );
}

/// The control: the detector names the shape the two reds had — a
/// deadline built beside the spawn, in the test or in a fixture thread
/// inside it — and passes the same test written through the helper, so
/// the green above is the tree's and not a detector that sees nothing.
#[test]
fn the_detector_names_the_shape_of_both_reds_and_passes_the_helper() {
    let prove_shape = r#"
        fn leg() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "x::y", "--nocapture"])
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while child.try_wait().unwrap().is_none() {
                assert!(std::time::Instant::now() < deadline);
            }
        }
    "#;
    let hits = bare_bounds(prove_shape).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].starts_with("fn leg: re-exec at [3]"), "{hits:?}");

    let server_shape = r#"
        async fn fetch() {
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                loop { assert!(Instant::now() < deadline); }
            });
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "x::y"]).output().unwrap();
        }
    "#;
    assert_eq!(bare_bounds(server_shape).unwrap().len(), 1);

    let channel_and_tokio = r#"
        fn a() { let c = Command::new(current_exe().unwrap()).args(["--exact", "t"]); rx.recv_timeout(d); }
        async fn b() { let c = Command::new(current_exe().unwrap()).args(["--exact", "t"]); tokio::time::timeout(d, c.wait()).await; }
    "#;
    assert_eq!(bare_bounds(channel_and_tokio).unwrap().len(), 2);

    // Review 12a4ee82, N1: the prove.rs shape written with a start and
    // `elapsed()`, either way round, and the `checked_add` deadline.
    let elapsed_and_checked_add = r#"
        fn a() {
            let mut child = Command::new(current_exe().unwrap()).args(["--exact", "t"]).spawn().unwrap();
            let spawned = Instant::now();
            while child.try_wait().unwrap().is_none() { assert!(spawned.elapsed() < Duration::from_secs(30)); }
        }
        fn b() {
            let c = Command::new(current_exe().unwrap()).args(["--exact", "t"]);
            let t = Instant::now();
            if Duration::from_secs(30) <= t.elapsed() { panic!(); }
        }
        fn c() {
            let c = Command::new(current_exe().unwrap()).args(["--exact", "t"]);
            let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
        }
    "#;
    let hits = bare_bounds(elapsed_and_checked_add).unwrap();
    assert_eq!(hits.len(), 3, "{hits:?}");

    // A duration READ for a message, with no comparison, is not a bound.
    let measured_only = r#"
        fn a() {
            let c = Command::new(current_exe().unwrap()).args(["--exact", "t"]);
            let sent = Instant::now();
            let took = sent.elapsed();
        }
    "#;
    assert!(bare_bounds(measured_only).unwrap().is_empty());

    // Not a re-exec of a TEST: the CLI resolving its own path to run a
    // verb (cadence.rs) names no `--exact`.
    let production = r#"
        fn verb() { let exe = std::env::current_exe().unwrap(); let d = Instant::now() + x; }
    "#;
    assert!(bare_bounds(production).unwrap().is_empty());

    let through_the_helper = r#"
        fn leg() {
            let mut run = boss_testing::reexec::ReExec::new("x::y").spawn();
            let done = run.finish(std::time::Duration::from_secs(30));
            let started = std::time::Instant::now();
        }
    "#;
    assert!(bare_bounds(through_the_helper).unwrap().is_empty());
}
