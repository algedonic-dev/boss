//! v2 content-address inputs are immutable model data, not an editable test harness.
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

#[test]
fn test_only_changes_preserve_production_and_the_original_v2_identifier() {
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
        "registered-agent-handoff-v2:da0c4a6ddef6fa9120bf8bc663c5d37bb13c1437d2106425d7cb28848a2c5bdf"
    );
}
