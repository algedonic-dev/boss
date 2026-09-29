//! A rule file edited without a version bump files an alarm at boot
//! (backlog 732c3cf9).
//!
//! WHY A PACKET AND NOT ONLY A LOG LINE. The seed compares every
//! authored rule with the row at the file's own version and names each
//! one that differs (`rules::seed::Drift`). On 2026-09-29 four such
//! rules — among them the org-admin GitHub token's per-request key —
//! sat for up to eleven days under a boot line that said "already
//! matches"; the defect was found by a refused ops-request, not by
//! anything reading the journal. A warning in a boot log is a check
//! nobody reads (CLAUDE.md §Diagnosis), so each drift also becomes ONE
//! urgent backlog-item, deduped on [`DRIFT_KEY`] = `<name>@v<version>`
//! against the OPEN alarms, the way the coverage backstop dedups on its
//! control.
//!
//! WHY IT NEVER STOPS THE BOOT. The seed runs before the rules runner
//! starts, and a boot that refused over one drifted file would stop
//! every rule — the arm needing the patient. So the dispatcher spawns
//! this and proceeds: an unreachable jobs API is retried a bounded
//! number of times and then said in the journal, and the next boot
//! tries again.
//!
//! WHAT CLOSES IT. The remedy is a car that bumps the file's version;
//! the seed then inserts the new version and the drift is gone. This
//! does not withdraw its own alarm — the triage that routes the fix
//! closes it — and a still-open alarm is never filed twice.

use std::collections::BTreeSet;

use boss_dispatcher::rules::handler::HandlerError;
use boss_dispatcher::rules::seed::Drift;
use boss_jobs::channels::InputChannel;
use serde_json::{Value as Json, json};

use super::common::{jobs_where, owner_for_filing, post_json, with_lane};

/// The name the filing signs as (`rule:<this>`): the boot seed is not a
/// rule row, but the jobs API credits every dispatcher write to one.
pub const ACTOR: &str = "dispatcher-rule-seed";

/// The packet-metadata key holding `<name>@v<version>` — the dedup key.
pub const DRIFT_KEY: &str = "drifted_rule";

/// The `scope` every alarm this files carries.
pub const SCOPE: &str = "rule-drift";

/// `<name>@v<version>`: one alarm per drifted version, so a second edit
/// that is bumped and drifts again is a new finding.
pub fn drift_key(d: &Drift) -> String {
    format!("{}@v{}", d.name, d.version)
}

/// The alarm for one drift: which rule, which version, which fields,
/// whether the old shape is being enforced right now, and the remedy.
pub fn alarm_body(d: &Drift, owner: &str) -> Json {
    let file = format!("infra/dispatcher/rules/{}.toml", d.name);
    let fields = d.fields.join(", ");
    let enforced = if d.status == "active" {
        "The row is ACTIVE, so the dispatcher is enforcing the OLD shape right now."
    } else {
        "The row is not active, so neither shape is enforced."
    };
    let detail = format!(
        "Found by the dispatcher's boot seed (backlog 732c3cf9): {file} declares version {v}, \
         and the dispatcher_rules row at version {v} differs from it in: {fields}. The file was \
         edited without a version bump, and the seed never rewrites a version that exists, so \
         the edit is NOT in effect. {enforced} Remedy: a car that sets `version = {next}` in \
         {file}; the next converge's seed inserts v{next} active and retires v{v}. Read the live \
         row with GET /api/dispatcher/rules/{name}/versions. \
         infra/lint/a-rule-edit-bumps-its-version.sh refuses such an edit at the gate; one that \
         reached the registry anyway predates it or bypassed it.",
        v = d.version,
        next = d.version + 1,
        name = d.name,
    );
    json!({
        "kind": "backlog-item",
        "title": format!(
            "RULE NOT LIVE: `{}` was edited at v{} without a version bump",
            d.name, d.version
        ),
        "subject": {"subject_kind": "custom", "id": d.name},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(json!({
            "area": "dispatcher",
            "scope": SCOPE,
            DRIFT_KEY: drift_key(d),
            "rule": d.name,
            "version": d.version,
            "row_status": d.status,
            "fields": d.fields,
            "description": detail,
        }), InputChannel::Telemetry),
    })
}

/// File one alarm per drift no open alarm already names. Returns how
/// many were filed. Reads the open alarms first and files nothing if
/// that read fails: a dark dedup read must not become a flood.
pub async fn file_drift_alarms(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    owner: &dyn boss_core::platform_owner::PlatformOwner,
    drifted: &[Drift],
) -> Result<usize, HandlerError> {
    if drifted.is_empty() {
        return Ok(0);
    }
    let jobs_base = jobs_base.trim_end_matches('/');
    let open: BTreeSet<String> = jobs_where(
        client,
        jobs_base,
        &format!("kind=backlog-item&status=open&metadata_has={DRIFT_KEY}"),
        ACTOR,
    )
    .await?
    .iter()
    .filter_map(|row| row.pointer(&format!("/metadata/{DRIFT_KEY}")))
    .filter_map(Json::as_str)
    .map(str::to_string)
    .collect();
    let owner = owner_for_filing(owner, ACTOR).await;
    let mut filed = 0usize;
    for d in drifted {
        if open.contains(&drift_key(d)) {
            continue;
        }
        post_json(
            client,
            &format!("{jobs_base}/api/jobs"),
            &alarm_body(d, &owner),
            ACTOR,
        )
        .await?;
        filed += 1;
    }
    Ok(filed)
}

/// The boot's spawn: [`file_drift_alarms`], retried a bounded number of
/// times, because at a fresh boot the jobs API may still be starting.
/// Every failure is said in the journal and nothing is returned — this
/// runs beside the dispatcher, never in front of it. A retry after a
/// partial filing re-reads the open alarms first, so it cannot file one
/// twice.
pub async fn file_at_boot(
    jobs_base: String,
    owner: std::sync::Arc<dyn boss_core::platform_owner::PlatformOwner>,
    drifted: Vec<Drift>,
) {
    const ATTEMPTS: usize = 10;
    const BETWEEN: std::time::Duration = std::time::Duration::from_secs(30);
    let client = super::common::api_client();
    for attempt in 1..=ATTEMPTS {
        match file_drift_alarms(&client, &jobs_base, owner.as_ref(), &drifted).await {
            Ok(filed) => {
                tracing::warn!(
                    drifted = drifted.len(),
                    filed,
                    "filed an alarm for each drifted dispatcher rule no open alarm names"
                );
                return;
            }
            Err(e) if attempt < ATTEMPTS => {
                tracing::warn!(attempt, error = %e, "filing the rule-drift alarms failed — retrying");
                tokio::time::sleep(BETWEEN).await;
            }
            Err(e) => tracing::error!(
                error = %e,
                drifted = ?drifted.iter().map(drift_key).collect::<Vec<_>>(),
                "could NOT file the rule-drift alarms; the drift is in the WARN lines above and \
                 the next boot tries again"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::api_client;
    use super::super::listing_stub;
    use super::*;

    const ALARMS: &str = "/api/jobs?kind=backlog-item&status=open&metadata_has=drifted_rule";

    fn drift(name: &str) -> Drift {
        Drift {
            name: name.into(),
            version: 1,
            status: "active".into(),
            fields: vec!["when", "do"],
        }
    }

    fn open_alarm(key: &str) -> Json {
        json!({"id": "al-1", "kind": "backlog-item", "status": "open",
               "metadata": {"scope": SCOPE, DRIFT_KEY: key}, "steps": []})
    }

    fn owner() -> boss_core::platform_owner::Fixed {
        boss_core::platform_owner::Fixed("emp-owner".into())
    }

    #[test]
    fn the_alarm_names_the_rule_the_fields_and_the_bump_that_fixes_it() {
        let body = alarm_body(&drift("broker-mints"), "emp-owner");
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m[DRIFT_KEY], "broker-mints@v1");
        assert_eq!(m["area"], "dispatcher");
        assert_eq!(m["fields"], json!(["when", "do"]));
        let detail = m["description"].as_str().unwrap();
        assert!(detail.contains("when, do"), "{detail}");
        assert!(detail.contains("`version = 2`"), "{detail}");
        assert!(detail.contains("enforcing the OLD shape"), "{detail}");
        assert!(
            body["title"].as_str().unwrap().contains("`broker-mints`"),
            "{}",
            body["title"]
        );
    }

    #[tokio::test]
    async fn each_drift_files_one_alarm() {
        let stub = listing_stub::serve(vec![(
            ALARMS,
            listing_stub::backlog_listing(&[], Some(DRIFT_KEY), Some(500)),
        )])
        .await;
        let filed = file_drift_alarms(
            &api_client(),
            &stub.base,
            &owner(),
            &[drift("a"), drift("b")],
        )
        .await
        .unwrap();
        assert_eq!(filed, 2);
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(sent.iter().all(|(route, _)| route == "POST /api/jobs"));
        assert_eq!(sent[0].1["metadata"][DRIFT_KEY], "a@v1");
    }

    /// Every boot re-reads the drift, so an alarm still open is not
    /// filed again — and the dedup read carries its `metadata_has`
    /// filter, so the open alarm is found behind a thousand unrelated
    /// backlog-items (the truncation c5ac71de measured).
    #[tokio::test]
    async fn an_open_alarm_for_the_drift_is_not_filed_again() {
        let stub = listing_stub::serve(vec![(
            ALARMS,
            listing_stub::backlog_listing(&[open_alarm("a@v1")], Some(DRIFT_KEY), Some(500)),
        )])
        .await;
        let filed = file_drift_alarms(&api_client(), &stub.base, &owner(), &[drift("a")])
            .await
            .unwrap();
        assert_eq!(filed, 0);
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// A dedup read that is not an answer files nothing: a flood of
    /// duplicates on every boot is worse than one missed boot.
    #[tokio::test]
    async fn a_dark_dedup_read_files_nothing() {
        let stub = listing_stub::serve(vec![(ALARMS, listing_stub::no_data_array())]).await;
        let err = file_drift_alarms(&api_client(), &stub.base, &owner(), &[drift("a")])
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::Downstream(_)), "{err:?}");
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn no_drift_reads_nothing_and_files_nothing() {
        let stub = listing_stub::serve(vec![]).await;
        let filed = file_drift_alarms(&api_client(), &stub.base, &owner(), &[])
            .await
            .unwrap();
        assert_eq!(filed, 0);
        assert!(stub.writes().is_empty());
    }
}
