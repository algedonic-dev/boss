//! The shared station selection transport for both executor pull commands.

use anyhow::{Context, Result, bail};
use boss_jobs::station_claim::SelectionReceipt;

#[derive(Debug)]
pub(crate) struct RecordedRefusal {
    original: String,
    request_id: uuid::Uuid,
}
impl std::fmt::Display for RecordedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "native station claim refused for request {}: {}",
            self.request_id, self.original
        )
    }
}
impl std::error::Error for RecordedRefusal {}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Pending {
    base: String,
    actor: String,
    pub station: String,
    pub request_id: uuid::Uuid,
    pub run_id: uuid::Uuid,
    receipt_json: Option<String>,
}

/// A local transport journal, not claim authority. The machine door
/// still authenticates every replay and returns the original receipt.
pub(crate) struct Journal {
    _lock: std::fs::File,
    path: std::path::PathBuf,
    base: String,
    actor: String,
    pending: Option<Pending>,
}

impl Journal {
    pub(crate) fn open(base: &str, actor: &str) -> Result<Self> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::OpenOptionsExt;
        let key = hex::encode(Sha256::digest(format!("{base}\0{actor}")));
        let root = if cfg!(test) {
            crate::own_temp::own_temp_path("boss-station-pull-tests")
        } else {
            dirs::state_dir()
                .context("station pull requires a local state directory")?
                .join("boss/station-pull")
        };
        std::fs::create_dir_all(&root).context("creating the station pull journal directory")?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(root.join(format!("{key}.lock")))?;
        lock.try_lock().map_err(|error| {
            anyhow::anyhow!("another station pull may own {actor}'s pending request: {error}")
        })?;
        let path = root.join(format!("{key}.json"));
        let pending: Option<Pending> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("reading station pull journal {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if pending
            .as_ref()
            .is_some_and(|pending| pending.base != base || pending.actor != actor)
        {
            bail!("station pull journal belongs to another instance or actor; nothing claimed");
        }
        Ok(Self {
            _lock: lock,
            path,
            base: base.into(),
            actor: actor.into(),
            pending,
        })
    }

    pub(crate) fn pending_station(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .map(|pending| pending.station.as_str())
    }

    fn save(&mut self, pending: Option<Pending>) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&pending)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &self.path)?;
        let parent = self
            .path
            .parent()
            .context("station pull journal has no parent")?;
        std::fs::File::open(parent)?.sync_all()?;
        self.pending = pending;
        Ok(())
    }

    pub(crate) fn request(&mut self, station: &str) -> Result<Pending> {
        if let Some(pending) = &self.pending {
            if pending.station != station {
                bail!(
                    "pending station pull {} belongs to {}; reconcile it before pulling {station}",
                    pending.request_id,
                    pending.station
                );
            }
            return Ok(pending.clone());
        }
        let pending = Pending {
            base: self.base.clone(),
            actor: self.actor.clone(),
            station: station.into(),
            request_id: uuid::Uuid::new_v4(),
            run_id: uuid::Uuid::new_v4(),
            receipt_json: None,
        };
        self.save(Some(pending.clone()))?;
        Ok(pending)
    }

    pub(crate) fn remember(&mut self, original: &str) -> Result<()> {
        let mut pending = self
            .pending
            .clone()
            .context("no pending station request for this receipt")?;
        if pending
            .receipt_json
            .as_ref()
            .is_some_and(|previous| previous != original)
        {
            bail!("native replay changed its original selection receipt; pending request retained");
        }
        pending.receipt_json = Some(original.into());
        self.save(Some(pending))
    }

    pub(crate) fn empty(&mut self) -> Result<()> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.receipt_json.is_some())
        {
            bail!("native replay answered empty after a recorded claim; pending request retained");
        }
        self.save(None)
    }

    pub(crate) fn refused(&mut self, refusal: &RecordedRefusal) -> Result<()> {
        use sha2::{Digest, Sha256};
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let digest = hex::encode(Sha256::digest(refusal.original.as_bytes()));
        let path = self
            .path
            .with_extension(format!("{}.{digest}.refused.json", refusal.request_id));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(refusal.original.as_bytes())?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(&path)? != refusal.original.as_bytes() {
                    bail!("retained native refusal differs; pending request retained");
                }
            }
            Err(error) => return Err(error.into()),
        }
        std::fs::File::open(&path)?.sync_all()?;
        std::fs::File::open(path.parent().context("refusal record has no parent")?)?.sync_all()?;
        self.save(None)
    }

    pub(crate) fn handed(&mut self) -> Result<()> {
        self.save(None)
    }
}

pub(crate) async fn claim_next_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    station: &str,
    actor: &str,
    request_id: uuid::Uuid,
) -> Result<Option<SelectionReceipt>> {
    let requested_station = station;
    let station = percent_encoding::utf8_percent_encode(station, crate::job::QUERY_VALUE);
    let (status, bytes) = crate::sor_client::api_response_at_signed_with(
        http,
        base,
        reqwest::Method::POST,
        &format!("/api/stations/{station}/claim-next"),
        Some(serde_json::json!({"request_id": request_id})),
        crate::identity::Signature::As(actor.to_owned()),
        &[],
    )
    .await
    .map_err(|error| anyhow::anyhow!("station pull request {request_id}: {error:#}"))?;
    if status == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    let original = String::from_utf8(bytes).context("selection receipt was not UTF-8")?;
    if status == reqwest::StatusCode::CONFLICT
        && let Ok(receipt) = SelectionReceipt::from_json(original.clone())
        && receipt["version"] == 2
        && receipt["outcome"] == "resource-refused"
        && receipt.get("owner_consumption_hash").is_some()
        && receipt.get("claimed_step").is_none()
        && receipt["selected"]["job_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
        && receipt["selected"]["step_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
        && receipt["refusal"].is_object()
        && receipt["refused_step"]["status"] == "ready"
        && receipt["refused_step"]["id"] == receipt["selected"]["step_id"]
        && receipt["refused_step"]["job_id"] == receipt["selected"]["job_id"]
        && matches_request(&receipt, request_id, requested_station, actor)
    {
        return Err(RecordedRefusal {
            original,
            request_id,
        }
        .into());
    }
    if status != reqwest::StatusCode::CREATED {
        bail!("station pull request {request_id} at {requested_station} -> {status}: {original}");
    }
    let receipt = SelectionReceipt::from_json(original)?;
    if !matches_request(&receipt, request_id, requested_station, actor) {
        bail!(
            "station pull request {request_id}: the returned original belongs to another request, station or actor; nothing filed"
        );
    }
    Ok(Some(receipt))
}

fn matches_request(
    receipt: &SelectionReceipt,
    request: uuid::Uuid,
    station: &str,
    actor: &str,
) -> bool {
    receipt["request_id"].as_str() == Some(request.to_string().as_str())
        && receipt["station"]["name"].as_str() == Some(station)
        && receipt["actor"].as_str() == Some(actor)
}

/// Only fresh server observations can retire a pending transport attempt.
pub(crate) async fn attempt_ended_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    actor: &str,
    pending: &Pending,
    receipt: &SelectionReceipt,
) -> Result<bool> {
    async fn get(
        http: &crate::machine_http::MachineClient,
        base: &str,
        actor: &str,
        path: &str,
    ) -> Result<Option<serde_json::Value>> {
        crate::sor_client::api_at_signed(
            http,
            base,
            reqwest::Method::GET,
            path,
            None,
            crate::identity::Signature::As(actor.to_owned()),
        )
        .await
    }
    match get(http, base, actor, &format!("/api/jobs/{}", pending.run_id)).await {
        Err(error) if format!("{error:#}").contains("-> 404 Not Found") => {}
        Err(error) => {
            return Err(error.context("reading the planned run during pull reconciliation"));
        }
        Ok(Some(run)) => {
            let run = run.get("data").unwrap_or(&run);
            if run["id"].as_str() != Some(pending.run_id.to_string().as_str())
                || run["kind"] != "agent-run"
            {
                bail!("planned run read returned a different record; pending request retained");
            }
            if run["status"] != "closed" {
                return Ok(false);
            }
        }
        Ok(None) => bail!("planned run read answered no record; pending request retained"),
    }
    let packet = receipt["selected"]["job_id"]
        .as_str()
        .context("original has no selected packet")?;
    let step_id = receipt["selected"]["step_id"]
        .as_str()
        .context("original has no selected step")?;
    let source = get(http, base, actor, &format!("/api/jobs/{packet}"))
        .await?
        .context("source read answered no record")?;
    if crate::envelope::job_id(&source) != Some(packet) {
        bail!("source read returned another packet; pending request retained");
    }
    let step = crate::envelope::steps(&source)
        .into_iter()
        .find(|step| step["id"] == step_id)
        .context("selected source step is unavailable; pending request retained")?;
    Ok(matches!(
        step["status"].as_str(),
        Some("ready" | "completed" | "skipped")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "{\n  \"version\": 1, \"actor\": \"agent-codex\", \"request_id\": \"f679ad30-8400-4440-8675-21e3863c97ea\",\n  \"station\": {\"name\": \"company\", \"id\": \"tree-root\"},\n  \"selected\": {\"job_id\": \"packet\", \"step_id\": \"step\"},\n  \"claimed_step\": {\"id\": \"step\", \"job_id\": \"packet\", \"spec_slug\": \"build\", \"status\": \"active\", \"assignee_id\": \"agent-codex\"}\n}";

    fn request_id() -> uuid::Uuid {
        uuid::Uuid::parse_str("f679ad30-8400-4440-8675-21e3863c97ea").unwrap()
    }

    #[tokio::test]
    async fn station_pull_keeps_the_original_receipt_bytes_and_its_own_actor() {
        let (base, served) = crate::sor_client::stub::one_response("201 Created", ORIGINAL).await;
        let http = crate::sor_client::machine_client().unwrap();
        let receipt = claim_next_at(&http, &base, "company", "agent-codex", request_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.json(),
            ORIGINAL,
            "the server artifact must be copied, not reserialized"
        );
        let request = served.await.unwrap().unwrap();
        assert!(request.starts_with("POST /api/stations/company/claim-next "));
        let user = request
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("x-boss-user"))
                    .map(|(_, value)| {
                        serde_json::from_str::<serde_json::Value>(value.trim()).unwrap()
                    })
            })
            .unwrap();
        assert_eq!(user["id"], "agent-codex");
        assert!(
            !request
                .to_ascii_lowercase()
                .contains(boss_jobs::runner_credential::HEADER)
        );
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["request_id"], request_id().to_string());
    }

    #[tokio::test]
    async fn station_pull_distinguishes_empty_from_a_native_refusal() {
        let http = crate::sor_client::machine_client().unwrap();
        let (base, served) = crate::sor_client::stub::one_response("204 No Content", "").await;
        assert!(
            claim_next_at(&http, &base, "company", "agent-codex", request_id())
                .await
                .unwrap()
                .is_none()
        );
        assert!(served.await.unwrap().is_some());
        let (base, served) =
            crate::sor_client::stub::one_response("409 Conflict", "native owner unavailable").await;
        let error = claim_next_at(&http, &base, "company", "agent-codex", request_id())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("native owner unavailable"));
        assert!(served.await.unwrap().is_some());
    }

    #[tokio::test]
    async fn station_pull_refuses_an_original_for_another_request_station_or_actor() {
        let http = crate::sor_client::machine_client().unwrap();
        for (station, actor, request) in [
            ("company", "agent-codex", uuid::Uuid::nil()),
            ("another-station", "agent-codex", request_id()),
            ("company", "agent-other", request_id()),
        ] {
            let (base, served) =
                crate::sor_client::stub::one_response("201 Created", ORIGINAL).await;
            let answer = claim_next_at(&http, &base, station, actor, request).await;
            assert!(
                answer.is_err(),
                "a different selection identity is not this request's claim"
            );
            assert!(served.await.unwrap().is_some());
        }
    }
}
