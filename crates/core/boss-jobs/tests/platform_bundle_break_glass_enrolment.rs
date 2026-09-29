//! `infra/platform/workflows/break-glass-enrolment.toml` keeps its
//! decided shape (design 03451237, David 2026-09-22). One pin file per
//! kind file (see `platform_bundle.rs`), so a new protocol touches no
//! shared line.
//!
//! The gateway reads this protocol by name and by step slug
//! (`boss_gateway::break_glass::{AUTHORISATION_KIND, AUTHORISE_STEP,
//! ENROL_STEP}`), and judges the `authorise` step's metadata keys
//! `label`, `rp_id`, `origin` and `decision`. Those names are the
//! contract between this file and that code; a rename here that the
//! gateway does not share authorises nothing, silently — so they are
//! pinned here, where the rename would be made.

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
fn the_break_glass_enrolment_keeps_its_decided_shape() {
    let lane = bundled("break-glass-enrolment");
    let step = |title: &str| {
        lane.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("break-glass-enrolment has a `{title}` step"))
    };

    // Q1: the authorisation is a presence-assured sign-off — producible
    // only by the gateway's verified WebAuthn assertion over the step's
    // shape — and a sign-off is owed, so the surface offers the ceremony
    // (backlog 148549c5: an assurance with no required role was
    // unreachable by the door that enforces it).
    let authorise = step("authorise");
    assert_eq!(authorise.kind, "sign-off");
    assert_eq!(authorise.assurance_required, Some(Assurance::Presence));
    assert_eq!(authorise.sign_offs_required, vec!["platform-admin"]);

    // The intent the signature covers: the key, the relying party and
    // the deployment are in THIS step's metadata (bound from the filer),
    // so the shape hash — and so the passkey — covers every one.
    for key in ["label", "rp_id", "origin"] {
        let field = authorise
            .fields
            .iter()
            .find(|f| f.name == key)
            .unwrap_or_else(|| panic!("`authorise` declares `{key}`"));
        assert!(
            field.required && field.filled_by == FilledBy::Filer,
            "`{key}` is supplied by the filer, so a packet without it is refused at admission"
        );
        let bound = authorise
            .metadata_defaults
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_eq!(
            bound,
            format!("{{metadata.{key}}}"),
            "`{key}` is bound into the signed step from the packet"
        );
    }
    let label = authorise.fields.iter().find(|f| f.name == "label").unwrap();
    assert_eq!(
        label.field_type, "primary|backup",
        "a break-glass key is primary or backup (Q2 of e9703d8f)"
    );
    assert!(
        authorise
            .fields
            .iter()
            .any(|f| f.name == "decision" && f.required && f.field_type == "approved|rejected"),
        "the approver's verdict is recorded, and only `approved` authorises"
    );

    // Q3: the `enrol` step is where the gateway writes the spend, and
    // it opens only on an approved authorisation.
    let enrol = step("enrol");
    assert!(
        enrol
            .ready_when
            .contains("steps.authorise.metadata.decision = \"approved\""),
        "nothing is enrolled without the approved decision"
    );
    // Backlog 4a173252: the spend is the WHOLE record — every field of
    // `boss_gateway::break_glass::BreakGlassCredential`, pinned on the
    // gateway side by `the_enrol_step_carries_every_field_of_the_record`
    // — so the step is the durable copy the commit car reads, not a pod
    // log the next converge replaces.
    for key in [
        "credential_id",
        "public_key",
        "sign_count",
        "label",
        "rp_id",
        "aaguid",
        "enrolled_at",
    ] {
        assert!(
            enrol.fields.iter().any(|f| f.name == key && f.required),
            "the spend records `{key}`"
        );
    }

    let terminals: Vec<&str> = lane
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    assert_eq!(terminals, vec!["enrolled", "declined"]);
}
