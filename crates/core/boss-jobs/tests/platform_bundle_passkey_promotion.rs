//! `infra/platform/workflows/passkey-promotion.toml` keeps its decided
//! shape (design 2cb6256f D2-D4, David 2026-09-28; backlog 2a228d0c).
//! One pin file per kind file (see `platform_bundle.rs`).
//!
//! The promotion's finish lives in the gateway: it reads this packet,
//! re-judges the `authorise` stamp against the step's current content,
//! takes an assertion from the key being promoted and one from a
//! break-glass hardware key, asks boss-people to flip the row, and
//! completes `promote`. It reads the protocol by these names — the
//! step slugs and the `authorise` metadata keys — so a rename here that
//! the gateway does not share authorises nothing, silently. They are
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
fn the_passkey_promotion_keeps_its_decided_shape() {
    let lane = bundled("passkey-promotion");
    let step = |title: &str| {
        lane.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("passkey-promotion has a `{title}` step"))
    };

    // The subject is the owner's employee Subject (D2).
    assert_eq!(lane.subject_kinds, vec!["employee"]);

    // D2: the approval is a presence-assured sign-off, and a sign-off is
    // owed, so the surface offers the ceremony (the break-glass
    // `authorise` shape, backlog 148549c5).
    let authorise = step("authorise");
    assert_eq!(authorise.kind, "sign-off");
    assert_eq!(authorise.assurance_required, Some(Assurance::Presence));
    assert_eq!(authorise.sign_offs_required, vec!["platform-admin"]);

    // What the passkey signs: the key's owner, its id, its label, when
    // it was enrolled and the fingerprint of its key material (so a key
    // re-registered under the same id promotes nothing — review of car
    // b3f6f5b4), bound into THIS step from the packet, so
    // the shape hash — and the signature — covers every one, and an
    // edit after signing leaves no live stamp.
    for key in [
        "employee_id",
        "credential_id",
        "label",
        "registered_at",
        "public_key_sha256",
    ] {
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
    assert!(
        authorise
            .fields
            .iter()
            .any(|f| f.name == "decision" && f.required && f.field_type == "approved|rejected"),
        "the approver's verdict is recorded, and only `approved` authorises"
    );

    // D3/D7: `promote` opens only on an approved authorisation, is a
    // role queue no agent is nominated to (the gateway completes it as
    // `automation:gateway`), and records what it spent.
    let promote = step("promote");
    assert_eq!(promote.kind, "task");
    assert!(
        promote
            .ready_when
            .contains("steps.authorise.metadata.decision = \"approved\""),
        "nothing is promoted without the approved decision"
    );
    assert!(
        promote.claimable == Some(true),
        "a role queue: the dispatcher nominates it to no agent"
    );
    for key in ["credential_id", "promoted_at", "vouched_by"] {
        assert!(
            promote.fields.iter().any(|f| f.name == key && f.required),
            "the spend records `{key}`"
        );
    }

    let terminals: Vec<&str> = lane
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    assert_eq!(terminals, vec!["promoted", "declined"]);
}
