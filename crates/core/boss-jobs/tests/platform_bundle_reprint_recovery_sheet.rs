//! `infra/platform/workflows/reprint-recovery-sheet.toml` keeps its
//! decided shape (design 125d405d §7, car 3; backlog fd6d6c08). One pin
//! file per kind file (see `platform_bundle.rs`).
//!
//! The daily check (`boss recovery reprint`, boss-cli's
//! recovery/reprint.rs) reads the version on paper off the `print` step
//! by name, and writes the keys the passkey signs. The decided shape is
//! the design's rule — a human act is a signature on rendered bytes,
//! never a transcription: the print step is a PRESENCE sign-off whose
//! every signed key the machine fills.

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
fn the_print_job_is_a_presence_signature_over_machine_filled_keys() {
    let lane = bundled("reprint-recovery-sheet");
    let step = |title: &str| {
        lane.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("reprint-recovery-sheet has a `{title}` step"))
    };
    let print = step("print");
    assert_eq!(print.kind, "sign-off");
    assert_eq!(print.assurance_required, Some(Assurance::Presence));
    assert_eq!(print.sign_offs_required, vec!["platform-admin"]);
    for key in [
        "fact_hash",
        "version",
        "pdf_sha256",
        "origin_sha",
        "rendered_at",
        "replaces_version",
        "statement",
    ] {
        let field = print
            .fields
            .iter()
            .find(|f| f.name == key)
            .unwrap_or_else(|| panic!("`print` declares `{key}`"));
        assert!(
            field.required && field.filled_by == FilledBy::Filer,
            "`{key}` is filled by the machine that files the print job, never typed"
        );
        assert_eq!(
            print
                .metadata_defaults
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
            format!("{{metadata.{key}}}"),
            "`{key}` is bound into the signed step from the packet"
        );
    }
    assert!(
        print
            .fields
            .iter()
            .any(|f| f.name == "decision" && f.required && f.field_type == "approved|rejected"),
        "only `approved` makes a version the paper copy"
    );
    assert_eq!(step("render").kind, "trigger");

    let terminals: Vec<&str> = lane
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    assert_eq!(terminals, vec!["printed", "declined"]);
}
