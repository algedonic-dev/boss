//! Layer-1 of the Job-completeness validator
//! (`docs/design/correctness-protocol.md`): every JobKindSpec in the
//! brewery seed bundle must pass the static lint. New JobKinds that bind
//! a side-effect-bearing StepType without filling required metadata fail
//! this test before publish — catching the bypass at authoring time
//! instead of waiting for a runtime "metadata X missing" log.
//!
//! Step-completion volumes, metadata shapes, and side-effect wiring are
//! validated end-to-end by the live 365-day regen, where the server
//! materializes steps and the workforce executor drives them through the
//! public API — not in-process here.

use std::path::PathBuf;

fn brewery_seeds_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("examples/brewery/seeds")
}

#[test]
fn brewery_job_kinds_pass_layer1_lint() {
    use boss_jobs::job_kind_lint::validate_all;
    use boss_jobs::seed_loader::load_job_kinds_with_owning_team;
    use boss_jobs::step_registry::StepRegistry;

    let kinds_path = brewery_seeds_dir().join("job_kinds.toml");
    let kinds = load_job_kinds_with_owning_team(&kinds_path, "brewery")
        .expect("brewery job_kinds.toml parses");
    let registry = StepRegistry::v1();
    let errs = validate_all(&kinds, &registry);
    if !errs.is_empty() {
        let mut msg = String::from("brewery JobKinds failed layer-1 lint:\n");
        for e in &errs {
            msg.push_str(&format!("  {}\n", e));
        }
        panic!("{msg}");
    }
}
