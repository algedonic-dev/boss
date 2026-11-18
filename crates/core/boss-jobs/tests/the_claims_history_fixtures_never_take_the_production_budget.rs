//! tree-wide pin — no test constructs the claims history's production
//! statement budget; a test that does is named by file and line.
//!
//! WHY (backlog ebbf38c6, 2026-11-10). Main gate 643e3baf at f150f78f went
//! RED on claims_console_history.rs's
//! cumulative_materialized_bytes_are_bounded_without_truncating_valid_waiting_claims
//! with `Unavailable("pre-commit statement budget expired")`, the gate's
//! drive having seen 5 s of io.full in 1465 s. The test judged what the
//! projection folds, and it ran on the Postgres claims history's `new` —
//! production's 2 s budget — so a stalled drive answered for the fold. Car
//! 21217d8a had already moved flow_resource_claims.rs onto the fixture seam
//! for the same red and left seventeen sites in three other files on
//! production's, which is how the red came back. Moving the sites is the
//! fix; this is what keeps them moved: a test reaches the adapter through
//! `for_fold_fixture` (or `for_fixture` with a budget it states) and
//! rebuilds through `rebuild_native_with`, never the adapter's `new` or
//! `rebuild_native`, which build production's.
//!
//! THE PRODUCTION SITES are named, each with its reason, and each must
//! still hold a hit, so an exemption that outlives its site is refused too.
//!
//! THE OTHER TWO 2 s BUDGETS those tests passed through (the second car
//! of ebbf38c6): the lease operations the jobs adapter runs, which a test
//! reaches only through its `for_fixture`, and the audit-tail read the
//! claims source makes, which spends the deadline its history answers, so
//! a test history advanced natively states one.
//!
//! The adapters' type names are spelled in pieces throughout: this file
//! reads source text and never the adapter, so it needs no `postgres`
//! feature, and a whole spelling would make a-pg-test-declares-its-feature
//! demand one.

use std::path::{Path, PathBuf};

/// The two calls that build the production budget. Spelled by `concat!`
/// so this file is not itself a hit.
const PRODUCTION_BUDGET: [&str; 2] = [
    concat!("Pg", "ClaimsHistory::", "new("),
    concat!("source::", "rebuild_native("),
];

/// The files that may build it, relative to the repo root, and why.
const PRODUCTION_SITES: [(&str, &str); 4] = [
    (
        "crates/core/boss-jobs/src/bin/boss_jobs_api.rs",
        "the serving adapter: production's budget is the claim",
    ),
    (
        "crates/orchestrators/boss-rebuild/src/main.rs",
        "the operator rebuild runs on production's budget",
    ),
    (
        "crates/core/boss-jobs/src/claims_console/postgres.rs",
        "the adapter's own tests pin the 2 s and its refusal of a spent clock",
    ),
    (
        "crates/core/boss-jobs/src/claims_console/source.rs",
        "`rebuild_native` itself, the production rebuild's entry",
    ),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every `<line>: <text>` in `source` that builds the production budget.
fn hits(source: &str) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| PRODUCTION_BUDGET.iter().any(|call| line.contains(call)))
        .map(|(n, line)| format!("{}: {}", n + 1, line.trim()))
        .collect()
}

#[test]
fn the_claims_history_fixtures_never_take_the_production_budget() {
    let root = boss_testing::repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    assert!(
        files.len() > 100,
        "the walk of crates/ found {} files; it is not reading the tree",
        files.len()
    );
    let mut offenders = Vec::new();
    let mut exempt_seen = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        let found = hits(&std::fs::read_to_string(file).unwrap_or_default());
        if found.is_empty() {
            continue;
        }
        if PRODUCTION_SITES.iter().any(|(site, _)| *site == rel) {
            exempt_seen.push(rel);
            continue;
        }
        offenders.extend(found.into_iter().map(|hit| format!("{rel}:{hit}")));
    }
    assert!(
        offenders.is_empty(),
        "these build the claims history's production 2 s statement budget outside a production \
         site; a test takes the adapter's for_fold_fixture (or for_fixture with a budget it \
         states) and source::rebuild_native_with (backlog ebbf38c6):\n{}",
        offenders.join("\n")
    );
    let stale: Vec<_> = PRODUCTION_SITES
        .iter()
        .filter(|(site, _)| !exempt_seen.iter().any(|seen| seen == site))
        .map(|(site, why)| format!("{site} ({why})"))
        .collect();
    assert!(
        stale.is_empty(),
        "these production sites no longer build the budget; drop the exemption:\n{}",
        stale.join("\n")
    );
}

/// What reaches a lease operation the jobs adapter runs (bind, sweep, discharge),
/// by method or by route. Spelled by `concat!` so this file is not itself
/// a reacher. `}/return` is the stock-return route's tail, which reaches
/// discharge through its handler (backlog 03a5dbdc): a route parameter
/// closes on the `}` whatever the test names it, `{}`, `{id}` or
/// `{acquisition}`.
const LEASE_REACH: [&str; 6] = [
    concat!("bind_resource_", "identity_at("),
    concat!("sweep_resource_", "leases_at("),
    concat!("discharge_", "stock_at("),
    concat!("/resource-", "identity"),
    concat!("resource-leases/", "sweep"),
    concat!("}/", "return"),
];
/// The jobs adapter's constructors that keep production's 2 s lease budget.
const PRODUCTION_LEASE_BUDGET: [&str; 3] = [
    concat!("Pg", "Jobs::", "new("),
    concat!("Pg", "Jobs::", "with_urls("),
    concat!("Pg", "Jobs::", "for_native_selection("),
];

/// Every test file under a `tests/` directory, with its text.
fn test_files() -> Vec<(String, String)> {
    let root = boss_testing::repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    assert!(
        files.len() > 100,
        "the walk of crates/ found {} files; it is not reading the tree",
        files.len()
    );
    files
        .into_iter()
        .filter(|f| f.components().any(|c| c.as_os_str() == "tests"))
        .map(|f| {
            let rel = f
                .strip_prefix(&root)
                .unwrap_or(&f)
                .to_string_lossy()
                .into_owned();
            (rel, std::fs::read_to_string(&f).unwrap_or_default())
        })
        .collect()
}

/// `<line>: <text>` for every line of `source` holding one of `needles`.
fn lines_with(source: &str, needles: &[&str]) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| needles.iter().any(|n| line.contains(n)))
        .map(|(n, line)| format!("{}: {}", n + 1, line.trim()))
        .collect()
}

/// `<file>:<line>: <text>` for every production-budget constructor in a
/// file that reaches a lease operation — the pin's whole judgement, so
/// its controls run the same path the tree does.
fn lease_offenders(files: &[(String, String)]) -> Vec<String> {
    files
        .iter()
        .filter(|(_, text)| !lines_with(text, &LEASE_REACH).is_empty())
        .flat_map(|(rel, text)| {
            lines_with(text, &PRODUCTION_LEASE_BUDGET)
                .into_iter()
                .map(move |hit| format!("{rel}:{hit}"))
        })
        .collect()
}

/// The lease half of backlog ebbf38c6: the jobs adapter's bind, sweep and discharge
/// spent a literal 2 s under every test, so a test judging what they write
/// lost to a loaded gate drive. A test file that reaches one builds its
/// jobs adapter with `for_fixture` (or states a budget through
/// `with_lease_budget`), never a constructor that keeps production's.
#[test]
fn the_lease_fixtures_never_take_the_production_budget() {
    let files = test_files();
    assert!(
        files
            .iter()
            .any(|(_, text)| !lines_with(text, &LEASE_REACH).is_empty()),
        "no test reaches a lease operation; the scan is not reading the tests it guards"
    );
    let offenders = lease_offenders(&files);
    assert!(
        offenders.is_empty(),
        "these test files reach a lease operation and build the jobs adapter on production's 2 s lease \
         budget; take its for_fixture (backlog ebbf38c6):\n{}",
        offenders.join("\n")
    );
}

/// The tail half: the claims source reads the audit tail on the deadline
/// its history answers, and a decorator that does not pass its inner
/// history's on gets the trait's production 2 s. A test file that
/// advances a history natively defines `source_deadline` once for every
/// history it implements.
#[test]
fn a_test_history_that_is_advanced_natively_states_its_source_deadline() {
    let impls = concat!("ClaimsHistory ", "for ");
    let advances = [
        concat!("advance_native_", "once("),
        concat!("source::", "run("),
        concat!("rebuild_native_", "with("),
    ];
    let deadline = concat!("fn source_", "deadline(");
    let offenders: Vec<_> = test_files()
        .into_iter()
        .filter(|(_, text)| !lines_with(text, &advances).is_empty())
        .filter(|(_, text)| text.matches(impls).count() > text.matches(deadline).count())
        .map(|(rel, text)| {
            format!(
                "{rel}: {} history impl(s), {} source_deadline",
                text.matches(impls).count(),
                text.matches(deadline).count()
            )
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "these advance a test history natively without stating its source deadline, so the \
         audit tail reads on production's 2 s (backlog ebbf38c6):\n{}",
        offenders.join("\n")
    );
}

/// The lease scan's control: a planted reacher on production's jobs adapter is
/// named, and the fixture beside it is not.
#[test]
fn the_lease_scan_names_a_planted_production_pgjobs() {
    let planted = concat!(
        "let jobs = Arc::new(Pg",
        "Jobs::for_fixture(db.pool.clone()));\n",
        "let jobs = Arc::new(Pg",
        "Jobs::",
        "new(db.pool.clone()));\n",
        "jobs.sweep_resource_",
        "leases_at(&stamp, 1).await\n",
    );
    assert_eq!(lines_with(planted, &LEASE_REACH).len(), 1);
    let found = lines_with(planted, &PRODUCTION_LEASE_BUDGET);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("2: "), "{found:?}");
}

/// The stock-return route's control (backlog 03a5dbdc, from review
/// b1cb3c76): `POST /api/claims/{acquisition}/return` reaches discharge
/// through the HTTP handler (`stock_return.rs`), so a test that POSTs
/// there names no lease method in its own text. Planted in the shape the
/// one test that POSTs there writes it (flow_resource_claims.rs,
/// `format!` over the claim id), beside a production jobs adapter, the
/// scan names that adapter — and without the return needle it names
/// nothing, which is the silence this control exists to refuse.
#[test]
fn the_lease_scan_names_a_production_pgjobs_beside_the_stock_return_route() {
    let planted = concat!(
        "let jobs = Arc::new(Pg",
        "Jobs::",
        "new(db.pool.clone()));\n",
        "let path = format!(\"/api/claims/{}/",
        "return\", claim.id);\n",
    );
    let found = lease_offenders(&[("planted.rs".to_string(), planted.to_string())]);
    assert_eq!(
        found.len(),
        1,
        "the return route reaches discharge; a production jobs adapter beside it must be named: {found:?}"
    );
    assert!(found[0].starts_with("planted.rs:1: "), "{found:?}");
}

/// The control: the scan sees a planted site in the shape the red had,
/// and passes the seam beside it.
#[test]
fn the_scan_names_a_planted_production_budget() {
    let planted = concat!(
        "let history = Pg",
        "ClaimsHistory::for_fold_fixture(db.pool.clone());\n",
        "let history = Pg",
        "ClaimsHistory::new(db.pool.clone());\n",
        "boss_jobs::claims_console::source::rebuild_native_with(&history, &db.pool)\n",
        "boss_jobs::claims_console::source::",
        "rebuild_native(&db.pool)\n",
    );
    let found = hits(planted);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0].starts_with("2: "), "{found:?}");
    assert!(found[1].starts_with("4: "), "{found:?}");
}
