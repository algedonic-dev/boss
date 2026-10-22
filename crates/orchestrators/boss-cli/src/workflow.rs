//! `boss workflow publish <kind> <spec.json | bundle.toml | bundle-dir>`
//! — publish a protocol version without the footgun. A `.toml` path is
//! a workflow bundle (a tenant's, or one platform kind file
//! `infra/platform/workflows/<kind>.toml`) and a directory is the
//! platform bundle itself (`infra/platform/workflows`): the `kind` row
//! is read with the platform seed's own loader, so the row a fresh
//! database seeds and the row a live one publishes are one definition.
//! The owning team is the tenant's when a `tenant.toml` around the
//! bundle names one, else the one the kind's row declares (every
//! platform kind file says `platform`), else the live row's (backlog
//! 6645addc; review 679220b5).
//!
//! WHY THIS IS A VERB. Publishing was a hand-assembled sequence, done
//! three times in one day for ship-a-change v20/v21/v22 and again for
//! v23: GET the active spec, mutate it, re-inject the fields a
//! publishable row needs, `_validate`, create the draft, publish, then
//! GET again to see what actually went live (f2c2ed14).
//!
//! IT CAUSED A LIVE REGRESSION, and the mechanism is worth stating
//! exactly. `POST /api/workflows/{kind}/publish` takes NO BODY — it
//! promotes whatever draft currently exists, and a `{"version": 20}`
//! body is ignored rather than honoured. On 2026-08-28 the draft-create
//! failed 422 for a missing field, so nothing was created, and the
//! follow-up publish promoted a STALE v16 draft left over from earlier
//! work. ship-a-change ran as v16 in production for about four minutes.
//! v16 has no `proven` step at all, so any car admitted in that window
//! would have shipped with no proof step. Zero were — luck, not design.
//!
//! So the load-bearing move here is not convenience. It is REFUSING TO
//! PUBLISH INTO A DIRTY REGISTRY: if a draft already exists that is not
//! the one this command just created, stop and say what is sitting
//! there, because that is precisely the state that promoted v16.
//!
//! The other half is that every step is CHECKED rather than assumed —
//! the draft is read back before it is promoted, and the active row is
//! read back after. A publish that reports success without confirming
//! what went live is how a regression stays invisible for four minutes.

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// A draft that is not ours is a refusal, not a warning.
///
/// Returns the version of any draft found, so the message can name it.
/// Pure: the whole point is that this decision is inspectable without a
/// live registry, since the failure it prevents happened in production.
pub(crate) fn blocking_draft(versions: &[Value], ours: Option<i32>) -> Option<i32> {
    versions
        .iter()
        .filter(|v| v.get("status").and_then(Value::as_str) == Some("draft"))
        .filter_map(|v| v.get("version").and_then(Value::as_i64).map(|n| n as i32))
        .find(|v| Some(*v) != ours)
}

/// What `publish` says when a draft that is not its own is sitting in
/// the registry. It names both resolutions and picks NEITHER: which one
/// applies depends on whether packets are pinned to the draft, and the
/// verb that removes or rejects it says so itself (backlog 50ded63d —
/// the sentence used to name only the discard, which a pinned draft
/// refuses, so the operator was sent to a door that does not open).
pub(crate) fn blocking_draft_refusal(kind: &str, stale: i32) -> String {
    format!(
        "a draft of {kind} v{stale} is already sitting in the registry, and publish \
         promotes WHATEVER DRAFT EXISTS — it takes no body and cannot be told which \
         one.\n  Publishing now would put v{stale} live instead of your spec. That is \
         exactly how ship-a-change went from v19 to v16 in production for four \
         minutes.\n  Resolve it first: `boss workflow discard {kind} {stale}` removes a \
         draft no packet ran; when packets are pinned to it (an experiment's candidate) \
         the discard refuses, and `boss workflow reject {kind} {stale}` keeps it for them \
         and takes it out of publish's way for good."
    )
}

/// Did the publish put live what we meant to put live?
///
/// Compares the version AND the step titles, because a version number
/// alone would not have caught the v16 regression — v16 is a perfectly
/// valid version, just the wrong protocol.
/// The workflow row a `GET` or `PUT /api/workflows/{kind}` answered, or
/// a refusal naming the read and what the body carried instead.
///
/// Backlog f2eac973. Both answer a BARE `WorkflowSpec` (boss-jobs
/// `get_kind` / `update_kind`: `Json(spec)`) — never an envelope, never
/// a list. The three reads in `publish` spelled
/// `.get("data").cloned().unwrap_or(v)` and then took the LAST element
/// of any array, so every 200 body was "the row": an error body went on
/// to fail as "carries no version", three steps from the read that
/// caused it. The row is read bare and must carry what every
/// `WorkflowSpec` does, a non-empty `kind` and an integer `version`.
pub(crate) fn workflow_row(body: Value, what: &str) -> Result<Value> {
    let is_row = body
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|k| !k.is_empty())
        && body.get("version").and_then(Value::as_i64).is_some();
    if is_row {
        return Ok(body);
    }
    let carried = match &body {
        Value::Object(m) => format!(
            "keys [{}]",
            m.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
        ),
        Value::Array(_) => "a list".to_string(),
        Value::Null => "null".to_string(),
        _ => "a scalar".to_string(),
    };
    bail!(
        "{what} answered no workflow row (no string `kind` and integer `version`; the body \
         carried {carried})"
    )
}

pub(crate) fn confirm(active: &Value, want_version: i32, want_titles: &[String]) -> Result<()> {
    let got_version = active
        .get("version")
        .and_then(Value::as_i64)
        .context("the active row carries no version")? as i32;
    let got_titles: Vec<String> = active
        .get("steps")
        .and_then(Value::as_array)
        .map(|ss| {
            ss.iter()
                .filter_map(|s| s.get("title").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if got_version != want_version {
        bail!(
            "PUBLISHED THE WRONG VERSION: meant v{want_version}, the active row is now \
             v{got_version}. This is the v19->v16 shape — publish promotes whatever draft \
             exists, so a stale draft can go live in place of yours."
        );
    }
    if got_titles != want_titles {
        bail!(
            "v{got_version} went live but its steps are not the ones published.\n  \
             live: {got_titles:?}\n  sent: {want_titles:?}"
        );
    }
    Ok(())
}

/// Fields a publishable row needs that a GET of the active row does not
/// hand back in usable form. Carrying them forward is four of the steps
/// this verb replaces, and forgetting one is the 422 that left the
/// stale draft armed.
pub(crate) fn carry_forward(spec: &mut Value, active: Option<&Value>) {
    let obj = match spec.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    obj.entry("status")
        .or_insert_with(|| Value::String("draft".into()));
    if !obj.contains_key("version") {
        obj.insert("version".into(), Value::from(1));
    }
    if let Some(a) = active {
        for k in ["created_at", "authoring_job_id", "owning_team"] {
            if !obj.contains_key(k)
                && let Some(v) = a.get(k)
            {
                obj.insert(k.into(), v.clone());
            }
        }
    }
    // A new kind with no team named anywhere is the platform's, as
    // `WorkflowSpec::platform_seed` says.
    obj.entry("owning_team")
        .or_insert_with(|| Value::String("platform".into()));
}

pub(crate) fn titles(spec: &Value) -> Vec<String> {
    spec.get("steps")
        .and_then(Value::as_array)
        .map(|ss| {
            ss.iter()
                .filter_map(|s| s.get("title").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Discard a draft version, then confirm it is GONE by reading the
/// version list back — a 204 is a claim; the read-back is the fact.
pub async fn discard(kind: &str, version: i32) -> Result<()> {
    let http = crate::sor_client::machine_client()?;
    crate::sor_client::api(
        &http,
        reqwest::Method::DELETE,
        &format!("/api/workflows/{kind}/versions/{version}"),
        None,
    )
    .await?;
    let versions = crate::train::rows(
        crate::sor_client::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}/versions"),
            None,
        )
        .await?,
    )?;
    let still_there = versions
        .iter()
        .any(|v| v.get("version").and_then(serde_json::Value::as_i64) == Some(i64::from(version)));
    if still_there {
        anyhow::bail!(
            "the DELETE answered but {kind} v{version} is still in the version list — \
             refusing to call that discarded"
        );
    }
    println!("boss workflow: {kind} v{version} draft discarded — confirmed gone");
    Ok(())
}

/// The status `version` holds in a version list, if it is there.
fn status_of(versions: &[Value], version: i32) -> Option<&str> {
    versions
        .iter()
        .find(|v| v.get("version").and_then(Value::as_i64) == Some(i64::from(version)))
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
}

/// Reject a draft that met traffic and lost (backlog 50ded63d), then
/// confirm it reads `rejected` — a 204 is a claim; the read-back is the
/// fact. Every refusal is the server's, passed through as it said it:
/// an experiment still splitting onto the version, history, a typo, or
/// a draft no packet ran (which the discard removes instead).
pub async fn reject(kind: &str, version: i32) -> Result<()> {
    let http = crate::sor_client::machine_client()?;
    crate::sor_client::api(
        &http,
        reqwest::Method::POST,
        &format!("/api/workflows/{kind}/versions/{version}/reject"),
        None,
    )
    .await?;
    let versions = crate::train::rows(
        crate::sor_client::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}/versions"),
            None,
        )
        .await?,
    )?;
    match status_of(&versions, version) {
        Some("rejected") => {}
        other => bail!(
            "the reject answered but {kind} v{version} reads {} in the version list — \
             refusing to call that rejected",
            other.unwrap_or("absent")
        ),
    }
    println!(
        "boss workflow: {kind} v{version} rejected — kept for the packets pinned to it, \
         never published; confirmed by reading the version list back"
    );
    Ok(())
}

/// Pick `kind` out of a parsed workflow bundle, as the JSON body the
/// draft endpoint takes. Pure, so the choice is testable without a
/// bundle on disk: a bundle that lacks the kind is a refusal naming
/// what it does hold, never a publish of the wrong protocol.
pub(crate) fn spec_from_bundle(
    kind: &str,
    specs: &[boss_jobs::registry::WorkflowSpec],
) -> Result<Value> {
    let spec = specs.iter().find(|s| s.kind == kind).with_context(|| {
        format!(
            "the bundle has no `{kind}`; it holds: {}",
            specs
                .iter()
                .map(|s| s.kind.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    serde_json::to_value(spec).context("rendering the bundle row as JSON")
}

/// The spec to publish: a JSON body, or — when the path ends in
/// `.toml` or is a bundle DIRECTORY — the `kind` row of a workflow
/// bundle read by the same loader the platform seed uses. So a bundled
/// protocol has ONE definition: the seed inserts it on a fresh
/// database, this verb publishes the identical row on a live one
/// (CLAUDE.md §9a). The platform bundle is a directory of kind files,
/// so `infra/platform/workflows` and
/// `infra/platform/workflows/<kind>.toml` both name the same row.
///
/// Two of the loader's columns are its own stamps, not the file's
/// claims, and are dropped so the live row's values carry forward:
/// `authoring_job_id`, which a bundle never holds, and `owning_team`
/// when neither a tenant manifest around the bundle nor the kind's own
/// row names one (backlog 6645addc — both rode a tenant row as
/// `platform`/null on 2026-10-09). A platform kind file's
/// `owning_team = "platform"` is a claim, and it is sent (review
/// 679220b5 F1): the loader refuses any other value there, so what the
/// row says is what the stamp said.
pub(crate) fn load_spec(kind: &str, path: &std::path::Path) -> Result<Value> {
    if path.is_dir() || path.extension().and_then(|e| e.to_str()) == Some("toml") {
        let owner = bundle_owner(path)?;
        let specs = boss_jobs::seed_loader::load_workflows_with_owning_team(
            path,
            owner.as_deref().unwrap_or("platform"),
        )
        .with_context(|| format!("reading the bundle {}", path.display()))?;
        let mut spec = spec_from_bundle(kind, &specs)?;
        let declared = owner.is_some() || declares_owning_team(kind, path)?;
        if let Some(obj) = spec.as_object_mut() {
            obj.remove("authoring_job_id");
            if !declared {
                obj.remove("owning_team");
            }
        }
        return Ok(spec);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading the spec {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("{} is not JSON", path.display()))
}

/// The team that owns a bundle's rows, read off the tenant around it:
/// the nearest ancestor of `path` carrying a `tenant.toml` (or
/// `seeds/tenant.toml`), whose `[meta] tenant_id` is what `boss tenant
/// check` stamps a tenant's workflows with. `None` for the platform
/// bundle, and for a slice cut out of its tenant into a scratch file,
/// which then takes the live row's owner (`carry_forward`). A manifest
/// that names no tenant refuses: read as "none", a new tenant kind went
/// out as the platform's with nothing said (review 679220b5 F2).
fn bundle_owner(path: &std::path::Path) -> Result<Option<String>> {
    let Ok(full) = path.canonicalize() else {
        return Ok(None);
    };
    let Some((dir, manifest)) = full
        .ancestors()
        .find_map(|d| crate::tenant::manifest_of(d).map(|m| (d.to_path_buf(), m)))
    else {
        return Ok(None);
    };
    crate::tenant::declared_tenant_id(&dir)
        .map(Some)
        .with_context(|| {
            format!(
                "{} is the tenant manifest around this bundle and names no `[meta] tenant_id` \
                 (unreadable, not TOML, or empty) — the bundle's owning team cannot be read, so \
                 nothing was published. Fix the manifest, or publish a JSON spec that names \
                 `owning_team`.",
                manifest.display()
            )
        })
}

/// Does `kind`'s own row in the bundle at `path` say `owning_team`?
/// Read off the file, because the loader's column is stamped either
/// way: the platform bundle's kind files say it, a tenant's rows do not.
/// A directory's row is `<dir>/<kind>.toml`, the loader's own rule.
fn declares_owning_team(kind: &str, path: &std::path::Path) -> Result<bool> {
    let file = if path.is_dir() {
        path.join(format!("{kind}.toml"))
    } else {
        path.to_path_buf()
    };
    let raw = std::fs::read_to_string(&file)
        .with_context(|| format!("reading {} for its owning_team", file.display()))?;
    let doc: toml::Value =
        toml::from_str(&raw).with_context(|| format!("{} is not TOML", file.display()))?;
    Ok(doc
        .get("workflow")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|w| w.get("kind").and_then(toml::Value::as_str) == Some(kind))
        .any(|w| w.get("owning_team").is_some()))
}

/// The live row `publish` builds on, out of the read of
/// `GET /api/workflows/{kind}`. A 404 is a new kind and answers `None`;
/// any other failure REFUSES (review 679220b5 F3). Read as "new kind",
/// a failed read sent the bundle's owner and no authoring job — the
/// live row is where both come from, so guessing it absent is the
/// 2026-10-09 mis-owning again by another road.
pub(crate) fn active_row(kind: &str, read: Result<Option<Value>>) -> Result<Option<Value>> {
    let what = format!("GET /api/workflows/{kind}");
    match read {
        Ok(body) => workflow_row(body.unwrap_or(Value::Null), &what).map(Some),
        Err(e) if format!("{e:#}").contains("-> 404 Not Found") => Ok(None),
        Err(e) => Err(e.context(format!(
            "{what} failed, so whether {kind} has a live row — the owner and authoring job a \
             publish carries forward — is unknown; nothing was published"
        ))),
    }
}

/// A publish that sends a different owning team than the live row
/// carries MOVES the kind between teams, which the drift lint reads as
/// a different author. Said, not refused: publishing from JSON naming
/// the team is how a mis-owned row is repaired (v3 of close-a-period).
pub(crate) fn owner_move_line(kind: &str, spec: &Value, active: Option<&Value>) -> Option<String> {
    let live = active?.get("owning_team")?.as_str()?;
    let sent = spec.get("owning_team")?.as_str()?;
    (live != sent).then(|| {
        format!(
            "boss workflow: {kind} CHANGES OWNER — the live row's owning_team is `{live}`, this \
             publish sends `{sent}`"
        )
    })
}

/// What the tree says about holding `kind` out of the unattended drift
/// publish, as this verb reports it (backlog 083d240e).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DriftHold {
    /// Held: `(source, why, lifts)`, the reader's own columns.
    Held(String, String, String),
    /// Not held (open, or released on the record).
    Open,
    /// The holds could not be read, in the reader's words.
    Unreadable(String),
}

/// The reader's relative path in a checkout — ONE reader for every
/// consumer (`infra/gcp/publish-drift.sh` is the one a hold binds), so
/// what this verb says about a hold is what that verb will do with it.
const HOLDS_READER: &str = "infra/gcp/workflow-holds.py";

/// Read `kind` out of `infra/gcp/workflow-holds.py`'s answer: its exit
/// status and its tab-separated stdout (`held|released <kind> <source>
/// <why> <lifts>`, or `problem <text>`). Pure, so the three answers are
/// pinned without a checkout.
pub(crate) fn drift_hold(kind: &str, reader_ok: bool, stdout: &str, stderr: &str) -> DriftHold {
    if !reader_ok {
        let said: Vec<&str> = stdout
            .lines()
            .filter_map(|l| l.strip_prefix("problem\t"))
            .chain(stderr.lines())
            .filter(|l| !l.trim().is_empty())
            .collect();
        return DriftHold::Unreadable(if said.is_empty() {
            "the hold reader failed and said nothing".to_string()
        } else {
            said.join("; ")
        });
    }
    // Every line is a whole `held` or `released` row, or the answer is
    // not one this verb can read — for ANY kind, since the row a broken
    // line belonged to is unknown (review 6ff3210e, N3). The other two
    // readers of this output refuse the same shape.
    let rows: Vec<Vec<&str>> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').collect())
        .collect();
    if let Some(odd) = rows
        .iter()
        .find(|c| !(c.len() == 5 && (c[0] == "held" || c[0] == "released")))
    {
        return DriftHold::Unreadable(format!(
            "the hold reader answered a line that is neither a held nor a released row: {}",
            odd.join("\t")
        ));
    }
    rows.into_iter()
        .find(|c| c[0] == "held" && c[1] == kind)
        .map(|c| DriftHold::Held(c[2].to_string(), c[3].to_string(), c[4].to_string()))
        .unwrap_or(DriftHold::Open)
}

/// The checkout whose holds describe this publish: the nearest ancestor
/// of the spec that carries the reader and a bundle, else of the working
/// directory — a ROLLBACK publishes an old row out of a scratch file, and
/// is run from the checkout.
fn holds_checkout(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let carries = |d: &std::path::Path| {
        d.join(HOLDS_READER).is_file() && d.join("infra/platform/workflows").is_dir()
    };
    let from = |start: std::path::PathBuf| {
        start
            .ancestors()
            .find(|d| carries(d))
            .map(|d| d.to_path_buf())
    };
    path.canonicalize()
        .ok()
        .and_then(from)
        .or_else(|| std::env::current_dir().ok().and_then(from))
}

/// Ask the tree whether `kind` is held out of the drift publish. `None`
/// when no checkout is around to ask — a JSON spec published from a
/// directory that is not a checkout — which the caller says rather than
/// reading as "not held".
pub(crate) fn read_drift_hold(kind: &str, path: &std::path::Path) -> Option<DriftHold> {
    drift_hold_in(&holds_checkout(path)?, kind)
}

/// The same question asked of a checkout the caller already holds — for
/// any other voice that would name this verb as a remedy (`boss
/// dispatch`'s refusal, backlog c6bd9f18). `None` when `repo` does not
/// carry the reader: not read, which is never "not held".
pub(crate) fn drift_hold_in(repo: &std::path::Path, kind: &str) -> Option<DriftHold> {
    if !repo.join(HOLDS_READER).is_file() {
        return None;
    }
    Some(
        match std::process::Command::new("python3")
            .arg(repo.join(HOLDS_READER))
            .arg(repo)
            .output()
        {
            Ok(out) => drift_hold(
                kind,
                out.status.success(),
                &String::from_utf8_lossy(&out.stdout),
                &String::from_utf8_lossy(&out.stderr),
            ),
            Err(e) => DriftHold::Unreadable(format!("python3 did not run: {e}")),
        },
    )
}

/// The lines this verb prints about a hold. A hold NEVER refuses this
/// verb: one kind, typed by an operator, is the deliberate act a hold
/// waits for, and the same act is the rollback. What it owes the
/// operator is the fact that the publish does not lift the hold, which
/// is what keeps the drift publish from undoing a rollback.
pub(crate) fn drift_hold_lines(kind: &str, hold: Option<&DriftHold>) -> Vec<String> {
    match hold {
        Some(DriftHold::Held(source, why, lifts)) => vec![
            format!(
                "boss workflow: {kind} is HELD out of the unattended drift publish ({source}): {why}"
            ),
            format!(
                "boss workflow: this hand publish is the deliberate act that hold waits for, and it does not lift it — {kind} STAYS HELD (neither publish-drift nor the publish-workflow verb will publish it, before or after a rollback) until a car changes the tree; lifts: {lifts}"
            ),
        ],
        Some(DriftHold::Open) => vec![],
        Some(DriftHold::Unreadable(said)) => vec![format!(
            "boss workflow: WARNING — the tree's drift-publish holds cannot be read ({said}); publish-drift refuses every run until a car fixes that. A hold does not bind this verb, so the publish goes on."
        )],
        None => vec![format!(
            "boss workflow: whether {kind} is held out of the unattended drift publish was NOT read — no checkout carrying {HOLDS_READER} around the spec or the working directory"
        )],
    }
}

pub async fn publish(kind: &str, path: &std::path::Path, dry: bool) -> Result<()> {
    let http = crate::sor_client::machine_client()?;
    let mut spec = load_spec(kind, path)?;
    let hold = read_drift_hold(kind, path);

    // The active row, for the fields a draft needs and for the
    // before/after comparison. A new kind answers 404; any other failed
    // read, and a 200 that is not a row, refuses (`active_row`).
    let active = active_row(
        kind,
        crate::sor_client::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}"),
            None,
        )
        .await,
    )?;
    carry_forward(&mut spec, active.as_ref());
    if let Some(line) = owner_move_line(kind, &spec, active.as_ref()) {
        println!("{line}");
    }
    let want_titles = titles(&spec);
    if want_titles.is_empty() {
        bail!("that spec declares no steps — refusing to publish a protocol with no work in it");
    }

    // REFUSE INTO A DIRTY REGISTRY, before writing anything.
    let versions = crate::train::rows(
        crate::sor_client::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}/versions"),
            None,
        )
        .await?,
    )?;
    if let Some(stale) = blocking_draft(&versions, None) {
        bail!("{}", blocking_draft_refusal(kind, stale));
    }

    // Lint before persisting — the same check the publish path enforces.
    let verdict = crate::sor_client::api(
        &http,
        reqwest::Method::POST,
        "/api/workflows/_validate",
        Some(spec.clone()),
    )
    .await?;
    let problems = verdict
        .as_ref()
        .and_then(|v| v.get("problems"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !problems.is_empty() {
        bail!("the spec does not lint clean, so nothing was written:\n  {problems:#?}");
    }
    println!(
        "boss workflow: {kind} lints clean ({} steps)",
        want_titles.len()
    );
    for line in drift_hold_lines(kind, hold.as_ref()) {
        println!("{line}");
    }

    if dry {
        println!("boss workflow: DRY — would create a draft and publish it");
        return Ok(());
    }

    // Create the draft, and CHECK IT LANDED. A failed create leaves the
    // publish armed against something else, which is the whole defect.
    let created = crate::sor_client::api(
        &http,
        reqwest::Method::PUT,
        &format!("/api/workflows/{kind}"),
        Some(spec.clone()),
    )
    .await?
    .map(|v| workflow_row(v, &format!("PUT /api/workflows/{kind}")))
    .transpose()?
    .context("the draft create returned no body — refusing to publish on that")?;
    let draft_version = created
        .get("version")
        .and_then(Value::as_i64)
        .context("the created draft carries no version — refusing to publish blind")?
        as i32;
    println!("boss workflow: draft v{draft_version} created — verified, not assumed");

    crate::sor_client::api(
        &http,
        reqwest::Method::POST,
        &format!("/api/workflows/{kind}/publish"),
        None,
    )
    .await?;

    // Read back what actually went live.
    let now_active = crate::sor_client::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/workflows/{kind}"),
        None,
    )
    .await?
    .map(|v| workflow_row(v, &format!("GET /api/workflows/{kind}")))
    .transpose()?
    .context("could not read the active row back")?;
    confirm(&now_active, draft_version, &want_titles)?;
    println!(
        "boss workflow: {kind} v{draft_version} is live, with the {} steps sent — confirmed by \
         reading the active row back",
        want_titles.len()
    );
    if matches!(hold, Some(DriftHold::Held(..))) {
        println!(
            "boss workflow: {kind} is still held out of the unattended drift publish — this publish did not lift the hold"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Backlog 083d240e. A hand publish of a kind the tree holds out of
    /// the drift publish SAYS so, and says the hold outlives it — the
    /// sentence that tells an operator a rollback will stick. It never
    /// refuses: the lines are advice beside a publish that goes on.
    #[test]
    fn a_hand_publish_of_a_held_kind_says_it_is_held_and_stays_held() {
        let held = "held\tops-request\tdeclared in infra/platform/workflow-holds/ops-request.toml\tturns on the signer refusal\tbacklog 6c9183de\nheld\tother\tby default: step x declares executor y\tw\tl\n";
        let hold = drift_hold("ops-request", true, held, "");
        assert_eq!(
            hold,
            DriftHold::Held(
                "declared in infra/platform/workflow-holds/ops-request.toml".into(),
                "turns on the signer refusal".into(),
                "backlog 6c9183de".into()
            )
        );
        let said = drift_hold_lines("ops-request", Some(&hold)).join("\n");
        for needle in [
            "ops-request is HELD out of the unattended drift publish",
            "turns on the signer refusal",
            "STAYS HELD",
            "does not lift it",
            "backlog 6c9183de",
        ] {
            assert!(said.contains(needle), "expected `{needle}` in:\n{said}");
        }

        // A kind the answer does not hold, and a released one, say nothing.
        assert_eq!(drift_hold("pr-train", true, held, ""), DriftHold::Open);
        let released = "released\tops-request\tdeclared in x\tcontrol passed\t-\n";
        assert_eq!(
            drift_hold("ops-request", true, released, ""),
            DriftHold::Open
        );
        assert!(drift_hold_lines("pr-train", Some(&DriftHold::Open)).is_empty());

        // Unreadable holds are a warning naming the problem, never
        // silence and never "not held"; no checkout is said as such.
        let bad = drift_hold(
            "ops-request",
            false,
            "problem\tinfra/platform/workflow-holds/ops-request.toml: `why` is required\n",
            "",
        );
        let said = drift_hold_lines("ops-request", Some(&bad)).join("\n");
        assert!(
            said.contains("cannot be read") && said.contains("`why` is required"),
            "{said}"
        );
        let said = drift_hold_lines("ops-request", None).join("\n");
        assert!(said.contains("was NOT read"), "{said}");
    }

    /// A reader that exits 0 with a line that is neither a whole `held`
    /// nor a whole `released` row did not finish its answer — a newline
    /// inside a writer value splits a held row in two (review 72485f08,
    /// F3). `registry-drift.sh` and the protocols lint read that as holds
    /// that cannot be read; this read it as `Open`, and `boss dispatch`'s
    /// refusal would then have named the publish for the very kind the
    /// broken row held (review 6ff3210e, N3). Half an answer is no answer,
    /// for every kind.
    #[test]
    fn an_answer_the_reader_did_not_finish_is_unreadable_never_open() {
        let split = "held\tops-request\tby default: step approve field decision declares writer sig\nner\tturns on a refusal\tbacklog 6c9183de\n";
        let unknown =
            "held\tpr-train\tdeclared in x\ta reason\tbacklog 6c9183de\nparked\tops-request\n";
        for (answer, said) in [(split, "declares writer sig"), (unknown, "parked")] {
            for kind in ["ops-request", "pr-train", "backlog-item"] {
                match drift_hold(kind, true, answer, "") {
                    DriftHold::Unreadable(why) => assert!(
                        why.contains(said),
                        "the line that could not be read is quoted: {why}"
                    ),
                    other => panic!("{kind}: expected unreadable, got {other:?}"),
                }
            }
        }
        // Blank lines are not an answer either way.
        assert_eq!(drift_hold("pr-train", true, "\n\n", ""), DriftHold::Open);
    }

    /// The same question asked of the REAL reader over a planted
    /// checkout: found from the spec's own path, and from a scratch
    /// file's working directory never (that case says "not read").
    #[test]
    fn the_hold_is_read_by_the_trees_one_reader_from_the_specs_checkout() {
        let has_tomllib = std::process::Command::new("python3")
            .args(["-c", "import tomllib"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_tomllib {
            eprintln!("skipping: the hold reader needs python3 with tomllib");
            return;
        }
        let root = boss_testing::scratch_dir("workflow-publish-hold");
        let bundle = root.join("infra/platform/workflows");
        let holds = root.join("infra/platform/workflow-holds");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(&holds).unwrap();
        std::fs::create_dir_all(root.join("infra/gcp")).unwrap();
        std::fs::copy(
            boss_testing::repo_root().join(HOLDS_READER),
            root.join(HOLDS_READER),
        )
        .unwrap();
        let row = |k: &str| format!("[[workflow]]\nkind = \"{k}\"\nlabel = \"{k}\"\n");
        boss_testing::write_file(&bundle.join("ops-request.toml"), &row("ops-request"));
        boss_testing::write_file(&bundle.join("pr-train.toml"), &row("pr-train"));
        boss_testing::write_file(
            &holds.join("ops-request.toml"),
            "drift_publish = \"held\"\nwhy = 'This row turns on the signer refusal on the approve path.'\nlifts = 'backlog 6c9183de'\n",
        );
        match read_drift_hold("ops-request", &bundle.join("ops-request.toml")) {
            Some(DriftHold::Held(source, why, lifts)) => {
                assert!(
                    source.contains("workflow-holds/ops-request.toml"),
                    "{source}"
                );
                assert!(why.contains("signer refusal"), "{why}");
                assert_eq!(lifts, "backlog 6c9183de");
            }
            other => panic!("expected the declared hold, got {other:?}"),
        }
        assert_eq!(
            read_drift_hold("pr-train", &bundle.join("pr-train.toml")),
            Some(DriftHold::Open)
        );
        // A hold that cannot be read is said, not swallowed.
        boss_testing::write_file(&holds.join("pr-train.toml"), "drift_publish = \"held\"\n");
        assert!(matches!(
            read_drift_hold("pr-train", &bundle.join("pr-train.toml")),
            Some(DriftHold::Unreadable(_))
        ));
    }

    /// Backlog f2eac973, the CLI neighbour. `GET` and `PUT
    /// /api/workflows/{kind}` answer a BARE `WorkflowSpec` (boss-jobs
    /// `get_kind` / `update_kind`: `Json(spec)`), never an envelope and
    /// never a list. The reads here spelled `.get("data").cloned()
    /// .unwrap_or(v)` and then took the last element of an array, so
    /// ANY 200 body was "the row": an envelope was unwrapped, a list
    /// was indexed, and an error body went on to a confusing "carries
    /// no version". The row is now read bare and must be one, and the
    /// refusal names what the body carried instead.
    #[test]
    fn a_workflow_read_takes_the_bare_row_or_refuses_naming_the_body() {
        let row = json!({"kind": "ship-a-change", "version": 22, "steps": []});
        assert_eq!(
            workflow_row(row.clone(), "GET /api/workflows/ship-a-change").unwrap(),
            row
        );
        for (body, carried) in [
            (json!({"data": row.clone()}), "keys [data]"),
            (json!({"error": "forbidden"}), "keys [error]"),
            (json!([row.clone()]), "a list"),
            (Value::Null, "null"),
            (json!({"kind": "ship-a-change"}), "keys [kind]"),
            (json!({"kind": "", "version": 3}), "keys [kind, version]"),
        ] {
            let why = workflow_row(body.clone(), "GET /api/workflows/ship-a-change")
                .expect_err("not a workflow row")
                .to_string();
            assert!(
                why.contains("GET /api/workflows/ship-a-change") && why.contains(carried),
                "{body}: {why}"
            );
        }
    }

    /// THE v16 REGRESSION, as a rule. A stale draft left by an earlier
    /// failed attempt is exactly what `publish` promotes, because it
    /// takes no body and cannot be told which version to promote.
    #[test]
    fn a_stale_draft_blocks_publishing() {
        let versions = vec![
            json!({"version": 30, "status": "active"}),
            json!({"version": 16, "status": "draft"}),
        ];
        assert_eq!(blocking_draft(&versions, None), Some(16));
    }

    /// ...and our own draft does not block us, or the verb could never
    /// publish anything.
    #[test]
    fn our_own_draft_is_not_a_blocker() {
        let versions = vec![
            json!({"version": 30, "status": "active"}),
            json!({"version": 31, "status": "draft"}),
        ];
        assert_eq!(blocking_draft(&versions, Some(31)), None);
    }

    #[test]
    fn a_clean_registry_has_no_blocker() {
        let versions = vec![json!({"version": 30, "status": "active"})];
        assert_eq!(blocking_draft(&versions, None), None);
    }

    /// Backlog 50ded63d. A candidate that met traffic and lost is kept
    /// as `rejected` — history its pinned packets still run — and the
    /// server's publish never promotes it, so it blocks nothing here.
    /// Before the status existed it stayed a draft forever (its pins
    /// forbid the discard) and this guard refused every later publish.
    #[test]
    fn a_rejected_candidate_is_not_a_blocker() {
        let versions = vec![
            json!({"version": 10, "status": "active"}),
            json!({"version": 11, "status": "rejected"}),
        ];
        assert_eq!(blocking_draft(&versions, None), None);
    }

    /// The refusal names BOTH resolutions, because the first one it
    /// named — the discard — is itself refused when a packet is pinned
    /// to the draft, and that refusal is where the kind used to stop.
    #[test]
    fn the_blocking_draft_refusal_names_the_discard_and_the_reject() {
        let said = blocking_draft_refusal("agent-run", 11);
        for needle in [
            "agent-run v11",
            "boss workflow discard agent-run 11",
            "boss workflow reject agent-run 11",
            "pinned",
        ] {
            assert!(said.contains(needle), "expected `{needle}` in:\n{said}");
        }
    }

    /// A bundle path publishes the row of the kind asked for, rendered
    /// as the JSON the draft endpoint takes — and a bundle without that
    /// kind refuses by name rather than publishing a neighbour.
    #[test]
    fn a_bundle_row_is_selected_by_kind_or_refused_by_name() {
        let specs = boss_jobs::seed_loader::parse_workflows(
            r#"
[[workflow]]
kind = "one"
label = "One"
category = "platform"
subject_kinds = ["custom"]
[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
[[workflow.step]]
title = "done"
kind = "outcome"
ready_when = "steps.opened.done"
terminal = { outcome = "completed" }

[[workflow]]
kind = "two"
label = "Two"
category = "platform"
subject_kinds = ["custom"]
[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
[[workflow.step]]
title = "done"
kind = "outcome"
ready_when = "steps.opened.done"
terminal = { outcome = "completed" }
"#,
            "platform",
            "inline",
        )
        .expect("the inline bundle parses and lints");

        let two = spec_from_bundle("two", &specs).expect("kind two is in the bundle");
        assert_eq!(two["kind"], json!("two"));
        assert_eq!(two["label"], json!("Two"));
        assert_eq!(
            titles(&two),
            vec!["opened".to_string(), "done".to_string()],
            "the steps ride along as JSON"
        );

        let e = spec_from_bundle("three", &specs).unwrap_err().to_string();
        assert!(e.contains("no `three`"), "{e}");
        assert!(
            e.contains("one, two"),
            "the refusal names what the bundle holds: {e}"
        );
    }

    /// The platform bundle is a DIRECTORY of kind files, and the verb
    /// takes either name for the same row: the directory (the row is
    /// picked by kind) or the one kind file (the row is the file). A
    /// kind file whose name lies about its kind is refused by the
    /// loader, not published under the wrong name.
    #[test]
    fn a_kind_file_and_its_directory_publish_the_same_row() {
        let row = |kind: &str| {
            format!(
                r#"[[workflow]]
kind = "{kind}"
label = "{kind}"
category = "platform"
subject_kinds = ["custom"]
[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
[[workflow.step]]
title = "done"
kind = "outcome"
ready_when = "steps.opened.done"
terminal = {{ outcome = "completed" }}
"#
            )
        };
        let dir = tempfile::tempdir().expect("a scratch bundle directory");
        std::fs::write(dir.path().join("one.toml"), row("one")).unwrap();
        std::fs::write(dir.path().join("two.toml"), row("two")).unwrap();
        std::fs::write(dir.path().join("README.md"), "# not a kind file\n").unwrap();

        // `platform_seed` stamps `created_at` at load time, so two loads
        // differ there and nowhere else — which is the comparison.
        let sans_stamp = |mut v: Value| {
            v.as_object_mut().map(|o| o.remove("created_at"));
            v
        };
        let from_dir = sans_stamp(load_spec("two", dir.path()).expect("the directory holds `two`"));
        let from_file = sans_stamp(
            load_spec("two", &dir.path().join("two.toml")).expect("the kind file IS `two`"),
        );
        assert_eq!(from_dir, from_file, "one definition, two names for it");
        assert_eq!(from_dir["kind"], json!("two"));

        let e = load_spec("three", dir.path()).unwrap_err().to_string();
        assert!(e.contains("no `three`") && e.contains("one, two"), "{e}");

        std::fs::write(dir.path().join("liar.toml"), row("truth")).unwrap();
        let e = format!("{:#}", load_spec("truth", dir.path()).unwrap_err());
        assert!(
            e.contains("liar.toml") && e.contains("expected kind `liar`"),
            "a file named for a kind it does not hold is refused by name: {e}"
        );
    }

    /// Backlog 6645addc, measured 2026-10-09T17:16Z: `boss workflow
    /// publish close-a-period <a slice of the tenant's workflows.toml>`
    /// put v2 live with owning_team `platform` (the live row said
    /// `algedonic`) and authoring_job_id null (the live row named
    /// aed7f81b), because the loader stamps `platform` on every row it
    /// reads and renders a null authoring job, and `carry_forward` fills
    /// only keys that are ABSENT. The pre-flight lint
    /// the-live-protocols-are-the-authored-protocols read red for 82 s
    /// until v3 went up from JSON. The body the draft PUT sends is
    /// checked here for both facets, in the two shapes a tenant row is
    /// published from: its tenant's checkout, and a slice cut out of it.
    #[test]
    fn a_tenant_bundle_publishes_under_its_owner_and_keeps_its_authoring_job() {
        let row = r#"[[workflow]]
kind = "close-a-period"
label = "Close a period"
category = "finance"
subject_kinds = ["custom"]
[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
[[workflow.step]]
title = "done"
kind = "outcome"
ready_when = "steps.opened.done"
terminal = { outcome = "completed" }
"#;
        let root = boss_testing::scratch_dir("workflow-publish-tenant-owner");
        let tenant = root.join("acme");
        std::fs::create_dir_all(tenant.join("seeds")).unwrap();
        std::fs::create_dir_all(root.join("slice")).unwrap();
        boss_testing::write_file(
            &tenant.join("tenant.toml"),
            "[meta]\ntenant_id = \"acme\"\n",
        );
        let in_checkout = tenant.join("seeds/workflows.toml");
        let slice = root.join("slice/close-a-period.toml");
        boss_testing::write_file(&in_checkout, row);
        boss_testing::write_file(&slice, row);

        let authored = "aed7f81b-4022-4e6e-83ad-051f5648a220";
        let active = json!({
            "kind": "close-a-period", "version": 1, "status": "active",
            "owning_team": "acme", "authoring_job_id": authored,
            "created_at": "2026-10-01T00:00:00Z",
        });
        // The body the draft PUT sends, built as `publish` builds it.
        let draft_body = |path: &std::path::Path, active: Option<&Value>| {
            let mut spec = load_spec("close-a-period", path).unwrap();
            carry_forward(&mut spec, active);
            spec
        };
        for path in [&in_checkout, &slice] {
            let body = draft_body(path, Some(&active));
            assert_eq!(
                owner_move_line("close-a-period", &body, Some(&active)),
                None
            );
            let sent: boss_jobs::registry::WorkflowSpec = serde_json::from_value(body)
                .expect("the draft body is a WorkflowSpec, as the PUT reads it");
            assert_eq!(sent.owning_team, "acme", "{}", path.display());
            assert_eq!(
                sent.authoring_job_id.map(|u| u.to_string()).as_deref(),
                Some(authored),
                "{}",
                path.display()
            );
        }

        // A NEW kind has no live row to carry: its tenant's checkout
        // names the owner, and a bare slice is the platform's.
        let new_kind = |path: &std::path::Path| {
            serde_json::from_value::<boss_jobs::registry::WorkflowSpec>(draft_body(path, None))
                .unwrap()
        };
        let fresh = new_kind(&in_checkout);
        assert_eq!(
            (fresh.owning_team.as_str(), fresh.authoring_job_id),
            ("acme", None)
        );
        let fresh = new_kind(&slice);
        assert_eq!(
            (fresh.owning_team.as_str(), fresh.authoring_job_id),
            ("platform", None)
        );

        // A checkout that names a DIFFERENT team than the live row moves
        // the kind, and the verb says so rather than doing it quietly.
        let other = json!({"kind": "close-a-period", "version": 1, "owning_team": "platform"});
        let body = draft_body(&in_checkout, Some(&other));
        let said = owner_move_line("close-a-period", &body, Some(&other)).expect("a move is said");
        assert!(
            said.contains("`platform`") && said.contains("`acme`"),
            "{said}"
        );
    }

    /// Review 679220b5, F1. The platform bundle's kind files each SAY
    /// `owning_team = "platform"` (all 69 on 2026-10-10), and that line
    /// is the tree's claim about who authors the kind. Dropping the
    /// loader's column whenever no tenant manifest is around threw that
    /// claim away with the stamp, so a platform kind mis-owned by a JSON
    /// publish stayed mis-owned through every bundle publish (the drift
    /// publish included), and nothing said so, because sent equalled
    /// live. A kind file that declares its owner sends it, and a move
    /// away from the live row's owner is said.
    #[test]
    fn a_platform_kind_file_sends_the_owner_it_declares_over_a_mis_owned_live_row() {
        let root = boss_testing::scratch_dir("workflow-publish-platform-owner");
        let bundle = root.join("infra/platform/workflows");
        std::fs::create_dir_all(&bundle).unwrap();
        boss_testing::write_file(
            &bundle.join("backlog-item.toml"),
            r#"[[workflow]]
kind = "backlog-item"
label = "Backlog item"
category = "platform"
subject_kinds = ["custom"]
owning_team = "platform"
[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
[[workflow.step]]
title = "done"
kind = "outcome"
ready_when = "steps.opened.done"
terminal = { outcome = "completed" }
"#,
        );
        let mis_owned = json!({
            "kind": "backlog-item", "version": 4, "status": "active",
            "owning_team": "acme", "authoring_job_id": "aed7f81b-4022-4e6e-83ad-051f5648a220",
        });
        for path in [bundle.clone(), bundle.join("backlog-item.toml")] {
            let mut body = load_spec("backlog-item", &path).unwrap();
            carry_forward(&mut body, Some(&mis_owned));
            assert_eq!(body["owning_team"], json!("platform"), "{}", path.display());
            let said = owner_move_line("backlog-item", &body, Some(&mis_owned))
                .unwrap_or_else(|| panic!("the move back to platform is said: {}", path.display()));
            assert!(
                said.contains("`acme`") && said.contains("`platform`"),
                "{said}"
            );
            // The authoring job is still the live row's: the file never names one.
            assert_eq!(
                body["authoring_job_id"],
                json!("aed7f81b-4022-4e6e-83ad-051f5648a220")
            );
        }
        // And the tree's own bundle, as publish-drift sends it.
        let real = boss_testing::repo_root().join("infra/platform/workflows");
        let mut body = load_spec("backlog-item", &real).unwrap();
        carry_forward(&mut body, Some(&mis_owned));
        assert_eq!(body["owning_team"], json!("platform"));
    }

    /// Review 679220b5, F3. The live row is where the owner and the
    /// authoring job come from, so only a 404 reads as "a new kind": a
    /// refused, dark or garbled read refuses the publish rather than
    /// sending the bundle's guess with no authoring job.
    #[test]
    fn only_a_404_reads_as_a_new_kind() {
        let row = json!({"kind": "close-a-period", "version": 2, "owning_team": "acme"});
        assert_eq!(
            active_row("close-a-period", Ok(Some(row.clone()))).unwrap(),
            Some(row)
        );
        let absent = anyhow::anyhow!(
            "jobs api GET /api/workflows/close-a-period -> 404 Not Found: no such workflow kind"
        );
        assert_eq!(active_row("close-a-period", Err(absent)).unwrap(), None);
        for failed in [
            "jobs api GET /api/workflows/close-a-period -> 403 Forbidden: denied",
            "jobs api GET /api/workflows/close-a-period -> 503 Service Unavailable: registry",
            "jobs api GET /api/workflows/close-a-period: connection refused",
        ] {
            let e = format!(
                "{:#}",
                active_row("close-a-period", Err(anyhow::anyhow!(failed)))
                    .expect_err("a failed read is not a new kind")
            );
            assert!(
                e.contains("nothing was published") && e.contains(failed),
                "the refusal keeps what the read said: {e}"
            );
        }
        // A 2xx with no body, or a body that is no row, is no row.
        assert!(active_row("close-a-period", Ok(None)).is_err());
        assert!(active_row("close-a-period", Ok(Some(json!({"error": "x"})))).is_err());
    }

    /// Review 679220b5, F2. A tenant manifest around the bundle that
    /// names no readable `[meta] tenant_id` is a broken tenant, not the
    /// absence of one: read as "no tenant", a NEW tenant kind went out
    /// stamped `platform` with nothing said. It refuses, naming the file.
    #[test]
    fn a_tenant_manifest_that_names_no_tenant_refuses_the_publish() {
        let root = boss_testing::scratch_dir("workflow-publish-broken-tenant");
        let tenant = root.join("acme");
        std::fs::create_dir_all(tenant.join("seeds")).unwrap();
        let rows = tenant.join("seeds/workflows.toml");
        boss_testing::write_file(
            &rows,
            r#"[[workflow]]
kind = "close-a-period"
label = "Close a period"
category = "finance"
subject_kinds = ["custom"]
[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
[[workflow.step]]
title = "done"
kind = "outcome"
ready_when = "steps.opened.done"
terminal = { outcome = "completed" }
"#,
        );
        for manifest in ["", "[meta]\n", "[meta]\ntenant_id = \"  \"\n", "not toml ["] {
            boss_testing::write_file(&tenant.join("tenant.toml"), manifest);
            let e = format!(
                "{:#}",
                load_spec("close-a-period", &rows)
                    .expect_err(&format!("a manifest reading {manifest:?} names no tenant"))
            );
            assert!(
                e.contains("tenant.toml") && e.contains("tenant_id"),
                "the refusal names the manifest and what it lacks: {e}"
            );
        }
    }

    /// CONFIRMATION COMPARES THE STEPS, NOT JUST THE NUMBER. v16 was a
    /// perfectly valid version — it was the wrong protocol, and a
    /// version check alone would have called that a success.
    #[test]
    fn a_valid_but_wrong_version_is_caught() {
        let live = json!({"version": 16, "steps": [{"title": "opened"}]});
        let want: Vec<String> = ["opened", "scope", "proven"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let e = confirm(&live, 31, &want).unwrap_err().to_string();
        assert!(e.contains("PUBLISHED THE WRONG VERSION"), "{e}");
        assert!(e.contains("v19->v16"), "{e}");
    }

    /// Right version, wrong contents — the case a version check misses
    /// entirely.
    #[test]
    fn the_right_version_with_the_wrong_steps_is_caught() {
        let live = json!({"version": 31, "steps": [{"title": "opened"}]});
        let want: Vec<String> = ["opened", "settled"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(confirm(&live, 31, &want).is_err());
    }

    #[test]
    fn a_faithful_publish_confirms() {
        let live = json!({"version": 31, "steps": [{"title": "opened"}, {"title": "settled"}]});
        let want: Vec<String> = ["opened", "settled"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(confirm(&live, 31, &want).is_ok());
    }
}
