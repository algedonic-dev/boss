//! v2 and v3 content-address inputs are immutable model data, not an editable test harness.
const CANONICAL: &str = include_str!("../src/moves/dispatch.rs");
const LIVE: &str = include_str!("../src/moves/dispatch_live.rs");
const BOUNDARY: &str = "\n#[cfg(test)]\nmod tests {\n";
const LIVE_SUFFIX: &str = "\n#[cfg(test)]\n#[path = \"dispatch_tests.rs\"]\nmod tests;\n";

fn production(source: &str) -> Result<&str, &'static str> {
    let mut parts = source.split(BOUNDARY);
    let prefix = parts.next().ok_or("production source missing")?;
    if parts.next().is_none() || parts.next().is_some() {
        return Err("expected exactly one explicit top-level test module boundary");
    }
    Ok(prefix)
}

fn same_production(canonical: &str, live: &str) -> Result<(), &'static str> {
    let prefix = live
        .strip_suffix(LIVE_SUFFIX)
        .ok_or("live tests must be one exact module suffix")?;
    if prefix.contains("#[cfg(test)]") || production(canonical)? != prefix {
        return Err("production changed: explicitly version the historical diagnosis model");
    }
    Ok(())
}

#[test]
fn live_dispatch_production_equals_the_immutable_v2_input() {
    same_production(CANONICAL, LIVE).unwrap();
}

#[test]
fn production_changes_and_missing_or_ambiguous_boundaries_refuse() {
    let changed = LIVE.replacen("run.kind !=", "run.kind ==", 1);
    assert!(same_production(CANONICAL, &changed).is_err());
    let missing = LIVE.replace(LIVE_SUFFIX, "\nmod tests;\n");
    assert!(same_production(CANONICAL, &missing).is_err());
    let ambiguous = format!("{LIVE}{LIVE_SUFFIX}");
    assert!(same_production(CANONICAL, &ambiguous).is_err());
    let appended = format!("{LIVE}pub fn unexpected_production() {{}}\n");
    assert!(same_production(CANONICAL, &appended).is_err());
}

/// The identifier ADVANCED, deliberately, once: v2 read a pinned version as
/// unavailable only when it was a `Draft`, so a rejected one passed (backlog
/// f9cbf891); v3 reads `was_published()`. `dispatch.rs` is byte-identical
/// under both, and v2's identifier is kept where its receipts are replayed
/// whole (`move_admission_observation_http.rs`). The next production change
/// to any hashed input is v4, never an edit of this string.
#[test]
fn test_only_changes_preserve_production_and_the_v3_identifier() {
    let tests = include_str!("../src/moves/dispatch_tests.rs");
    let changed = tests.replacen(
        "internal_protocols: None,",
        "internal_protocols: Some(control),",
        1,
    );
    assert_ne!(changed, tests);
    same_production(CANONICAL, LIVE).unwrap();
    assert_eq!(
        boss_jobs::move_diagnosis::model(),
        "registered-agent-handoff-v3:f37435b887121bfea4806304d68f2f7e0817fd288a13f2338622dc3a0d7183a6"
    );
}
