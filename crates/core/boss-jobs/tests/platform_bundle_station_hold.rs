//! `infra/platform/workflows/station-hold.toml` keeps the shape the gate
//! launchers and `boss gates hold` / `release` read (backlog 95401d82).
//! One pin file per kind file (see `platform_bundle.rs`), so a new
//! protocol touches no shared line.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// Open is held: the hold's ONLY ready step is `release`, which `boss
/// gates release` completes with its reason and its actor, and whose
/// completion closes the packet `released`. Nothing else can close it,
/// so a hold never lifts itself on a timer.
#[test]
fn a_station_hold_is_open_until_its_release_step_closes_it() {
    let wf = bundled("station-hold");
    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("station-hold has no `{title}` step"))
    };
    let release = step("release");
    assert_eq!(release.ready_when, "true");
    for name in ["released_why", "released_by"] {
        let f = release
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("release has no `{name}` field"));
        assert!(f.required, "{name} is required at done");
    }
    let released = step("released");
    assert_eq!(released.kind, "outcome");
    assert_eq!(released.ready_when, "steps.release.done");
    assert_eq!(
        released.terminal.as_ref().map(|t| t.outcome.as_str()),
        Some("released")
    );
    assert_eq!(wf.steps.len(), 2, "no other way out of a hold");
}
