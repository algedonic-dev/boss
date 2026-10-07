//! A branch's exclusions script is executable code, just like its lints.
use boss_testing::repo_root;

#[test]
fn the_conductor_does_not_execute_either_branch_entrypoint_locally() {
    let source = std::fs::read_to_string(
        repo_root().join("crates/orchestrators/boss-cli/src/train/conductor.rs"),
    )
    .expect("conductor source");
    assert!(
        !source.contains("consist_check(Path::new(clone)"),
        "the branch still executes in the credential-holding conductor"
    );
    assert!(
        source.contains("isolated_consist_check("),
        "both branch entrypoints need the isolated transport"
    );
}
