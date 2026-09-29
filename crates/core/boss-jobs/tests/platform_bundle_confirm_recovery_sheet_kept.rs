//! `infra/platform/workflows/confirm-recovery-sheet-kept.toml` keeps
//! its decided shape (design 125d405d Q2, answered by David 2026-09-28:
//! "reprint on every change, plus a 90-day existence check"). One pin
//! file per kind file (see `platform_bundle.rs`).
//!
//! The daily check (boss-cli's recovery/reprint.rs) files it and reads
//! its `confirm` step back: `approved` restarts the 90-day clock, and
//! anything else closes it `missing`, which files a reprint.

use boss_core::job::{Assurance, FilledBy};
use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

#[test]
fn the_existence_check_is_a_presence_signature_that_can_say_missing() {
    let lane = bundled("confirm-recovery-sheet-kept");
    let confirm = lane
        .steps
        .iter()
        .find(|s| s.title == "confirm")
        .expect("a `confirm` step");
    assert_eq!(confirm.kind, "sign-off");
    assert_eq!(confirm.assurance_required, Some(Assurance::Presence));
    assert_eq!(confirm.sign_offs_required, vec!["platform-admin"]);
    for key in [
        "fact_hash",
        "version",
        "print_packet",
        "printed_at",
        "statement",
    ] {
        assert!(
            confirm
                .fields
                .iter()
                .any(|f| f.name == key && f.required && f.filled_by == FilledBy::Filer),
            "`{key}` is filled by the check that files it"
        );
    }
    let terminals: Vec<&str> = lane
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    assert_eq!(terminals, vec!["kept", "missing"]);
}
