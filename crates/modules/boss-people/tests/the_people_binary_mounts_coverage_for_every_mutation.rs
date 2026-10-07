//! The API binary is the production constructor boundary: EVERY People
//! repository it constructs, and the passkey router, must receive the
//! same coverage reader.
//!
//! Until review c3b96c09 (F5, 2026-10-06) this pin counted exactly two
//! `.with_coverage(coverage.clone())` strings, so a third
//! `PgPeople::new(pool)` mounted unguarded beside them still passed. It
//! now derives the constructors from the source and holds each one to
//! its guard, and proves on a planted third constructor that it refuses.

const BINARY: &str = include_str!("../src/bin/boss_people_api.rs");
const GUARD: &str = ".with_coverage(coverage.clone())";

/// The end of the call whose opening parenthesis is at `open`.
fn call_end(source: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (offset, c) in source[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return open + offset + 1;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced call at byte {open}");
}

/// How many `PgPeople::<constructor>(…)` calls `source` makes, and each
/// one NOT immediately followed by the guard, as its source text.
fn unguarded_repositories(source: &str) -> (usize, Vec<String>) {
    let mut found = 0;
    let mut unguarded = Vec::new();
    let mut rest = 0;
    while let Some(at) = source[rest..].find("PgPeople::") {
        let start = rest + at;
        let open = start + source[start..].find('(').expect("a constructor call");
        let end = call_end(source, open);
        found += 1;
        if !source[end..].trim_start().starts_with(GUARD) {
            unguarded.push(source[start..end].split_whitespace().collect::<String>());
        }
        rest = end;
    }
    (found, unguarded)
}

#[test]
fn every_repository_the_binary_constructs_and_the_passkey_router_are_guarded() {
    let (found, unguarded) = unguarded_repositories(BINARY);
    assert!(
        found >= 2,
        "the CRUD and the workflow repositories are both constructed here; found {found}"
    );
    assert!(
        unguarded.is_empty(),
        "a People repository mounted without the coverage guard: {unguarded:?}"
    );
    assert!(
        !BINARY.contains("InMemoryPeople"),
        "the binary serves the Postgres repository only"
    );
    assert!(
        BINARY.contains("webauthn_router_with_coverage(") && !BINARY.contains("webauthn_router("),
        "passkey removal must use the guarded router, and only it"
    );
    let at = BINARY.find("webauthn_router_with_coverage(").unwrap();
    let mount = &BINARY[at..call_end(BINARY, at + "webauthn_router_with_coverage".len())];
    assert!(
        mount.contains("Some(coverage.clone())"),
        "passkey mutation receives the same coverage reader: {mount}"
    );
    assert!(
        BINARY.contains("HttpCoverageRead::new(policy_url.clone(), jobs_url.clone())"),
        "guard reads the actual configured services"
    );
}

/// The pin's own control: a third repository constructed unguarded, in
/// either spelling, is named — which the string count never did (the
/// count of guards is still two on this planted source).
#[test]
fn a_third_repository_mounted_unguarded_is_refused() {
    let planted = format!(
        "{BINARY}\nfn extra(pool: sqlx::PgPool) {{ let _ = std::sync::Arc::new(boss_people::PgPeople::new(pool.clone())); }}\n"
    );
    assert_eq!(
        planted.matches(GUARD).count(),
        BINARY.matches(GUARD).count()
    );
    let (found, unguarded) = unguarded_repositories(&planted);
    assert_eq!(found, unguarded_repositories(BINARY).0 + 1);
    assert_eq!(unguarded, vec!["PgPeople::new(pool.clone())".to_string()]);

    let registries = "boss_people::PgPeople::with_registries(\n    pool.clone(),\n    classes(a, b),\n)\n.with_departments(x)";
    assert_eq!(unguarded_repositories(registries).1.len(), 1);
    let guarded = "PgPeople::with_registries(pool.clone(), classes(a, b))\n        .with_coverage(coverage.clone())";
    assert_eq!(unguarded_repositories(guarded), (1, vec![]));
}
