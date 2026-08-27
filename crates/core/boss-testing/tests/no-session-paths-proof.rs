//! Proof fixture for the `no-session-paths` lint.
//!
//! The lint forbids tracked source from naming a machine-local path.
//! Proving it works requires a tracked file that DOES name one — this
//! file — which is exempt by construction (infra/lint/lib/
//! pattern-scan.sh: a tracked path that names the lint and lives
//! where tests live). The strings below are the point, not an
//! accident: `/Users/someone/scratch` and `.claude/jobs/abc/tmp`.
//!
//! The test asserts the fixture still carries them, because a fixture
//! that quietly lost its forbidden strings would keep passing while
//! proving nothing at all.

#[test]
fn the_fixture_still_names_what_it_is_exempt_for() {
    let me = include_str!("no-session-paths-proof.rs");
    assert!(
        me.contains("/Users/someone/"),
        "fixture no longer names a home-directory path — it proves nothing"
    );
    assert!(
        me.contains(".claude/jobs/"),
        "fixture no longer names a session scratch path — it proves nothing"
    );
}
