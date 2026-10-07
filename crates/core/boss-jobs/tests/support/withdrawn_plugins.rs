use boss_jobs::StepPluginSpec;

/// Immutable pre-withdrawal declarations copied from source1c42. They are
/// historical test data, outside every platform seed path. Pg migrations
/// independently pin every column; neither test derives expected rows from DB.
pub fn declarations() -> Vec<StepPluginSpec> {
    boss_jobs::seed_loader::load_step_plugins(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/withdrawn-step-plugins"
    ))
    .expect("historical declarations parse")
}
