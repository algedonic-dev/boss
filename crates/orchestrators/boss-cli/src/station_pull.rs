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

/// A refusal the server ANSWERED (a 4xx) that carries no receipt. It
/// proves only that this send committed nothing; whether an earlier send
/// of the same request, whose answer was lost, committed is a question
/// for the receipt door ([`receipt_at`]), never for the refusal's text —
/// the native read raises "before commit dispatch" ahead of ever reading
/// the store (backlog 614c2bb7). A 5xx is not one: it may be the commit
/// whose outcome is uncertain, so it keeps the request unasked.
#[derive(Debug)]
pub(crate) struct AnsweredRefusal {
    request_id: uuid::Uuid,
    station: String,
    status: reqwest::StatusCode,
    original: String,
}
impl std::fmt::Display for AnsweredRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "station pull request {} at {} -> {}: {}",
            self.request_id, self.station, self.status, self.original
        )
    }
}
impl std::error::Error for AnsweredRefusal {}

/// Whether a receipt's `actor` names this pull: the spelling it signs as,
/// or the id the agents registry ties to that spelling (`registered`,
/// resolved by the caller through `dispatch::resolve_agent`, never typed).
/// The claim door records an agent's REGISTERED id as the claimant, so an
/// agent signing as its alias gets back an original naming its id
/// (backlog 9bd65d7c: `claude@algedonic.dev` pulled, the store said
/// `agent-claude`, and a literal compare refused the pull's own claim).
pub(crate) fn names_puller(found: Option<&str>, actor: &str, registered: Option<&str>) -> bool {
    found.is_some_and(|found| found == actor || Some(found) == registered)
}

/// What the receipt door says the store holds for one request.
#[derive(Debug)]
pub(crate) enum ReceiptRead {
    /// The door's own 404, naming this request: nothing committed under it.
    None,
    /// A stored original for this request and actor: a claim to replay.
    Recorded,
}

/// Ask the server's receipt door what the store holds for `request_id`
/// (`GET /api/stations/claims/{id}`, backlog 614c2bb7). Only the door's
/// own 404 — a JSON body naming this request with no receipt — reads as
/// [`ReceiptRead::None`]. A bare 404 is a server without the door, and a
/// wrong target answers instead of erroring, so it is an error like any
/// other unreadable answer, and the caller keeps its request.
pub(crate) async fn receipt_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    actor: &str,
    registered: Option<&str>,
    request_id: uuid::Uuid,
) -> Result<ReceiptRead> {
    let (status, bytes) = crate::sor_client::api_response_at_signed_with(
        http,
        base,
        reqwest::Method::GET,
        &format!("/api/stations/claims/{request_id}"),
        None,
        crate::identity::Signature::As(actor.to_owned()),
        &[],
    )
    .await?;
    let body = String::from_utf8_lossy(&bytes);
    match status {
        reqwest::StatusCode::NOT_FOUND => {
            let answer: serde_json::Value = serde_json::from_slice(&bytes).with_context(|| {
                format!("the receipt door answered 404 without its own body ({body}); is it served here?")
            })?;
            // An explicitly PRESENT null, never an indexed one: serde_json
            // reads a missing member as Null, so an error body naming the
            // request would otherwise end it (review a5cd0260).
            if answer["request_id"].as_str() != Some(request_id.to_string().as_str())
                || answer.get("receipt") != Some(&serde_json::Value::Null)
            {
                bail!("the receipt door's 404 names another request or a receipt: {body}");
            }
            Ok(ReceiptRead::None)
        }
        reqwest::StatusCode::OK => {
            let receipt = SelectionReceipt::from_json(body.into_owned())?;
            if receipt["request_id"].as_str() != Some(request_id.to_string().as_str())
                || !names_puller(receipt["actor"].as_str(), actor, registered)
            {
                bail!("the receipt door returned an original for another request or actor");
            }
            Ok(ReceiptRead::Recorded)
        }
        other => bail!("the receipt door for {request_id} answered {other}: {body}"),
    }
}

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
    lock: std::fs::File,
    path: std::path::PathBuf,
    base: String,
    actor: String,
    pending: Option<Pending>,
}

/// AN EXPLICIT UNLOCK, NOT A BARE CLOSE (backlog cd70d4b0). A flock
/// belongs to the open file description, and a fork anywhere in the
/// process copies that description into the child until it execs; a
/// close releases the lock only once every copy is gone, an unlock
/// releases it for all of them. Gate-run 4cf59a86 read a test's own
/// dropped journal as "another station pull" because a concurrent test
/// held a forked child in pre_exec.
impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
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
            lock,
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

    /// End a request the receipt door holds nothing for. One
    /// that already returned a receipt keeps it: the server answering
    /// "none" for it is a contradiction to read, not a claim to forget.
    pub(crate) fn unclaimed(&mut self) -> Result<()> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.receipt_json.is_some())
        {
            bail!(
                "the server read no receipt for a request that recorded one; pending request retained"
            );
        }
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
    registered: Option<&str>,
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
        && matches_request(&receipt, request_id, requested_station, actor, registered)
    {
        return Err(RecordedRefusal {
            original,
            request_id,
        }
        .into());
    }
    if status != reqwest::StatusCode::CREATED {
        if status.is_client_error() {
            return Err(AnsweredRefusal {
                request_id,
                station: requested_station.into(),
                status,
                original,
            }
            .into());
        }
        bail!("station pull request {request_id} at {requested_station} -> {status}: {original}");
    }
    let receipt = SelectionReceipt::from_json(original)?;
    if !matches_request(&receipt, request_id, requested_station, actor, registered) {
        // The refusal names what the original said, so the next reader
        // does not have to fetch it from the store to learn which half
        // disagreed (backlog 9bd65d7c took a receipt-door read to learn).
        bail!(
            "station pull request {request_id} at {requested_station} as {actor}{}: the returned \
             original belongs to another request, station or actor (request {}, station {}, \
             actor {}); nothing filed",
            registered.map_or(String::new(), |id| format!(" ({id})")),
            receipt["request_id"],
            receipt["station"]["name"],
            receipt["actor"],
        );
    }
    Ok(Some(receipt))
}

/// Whether an original is this pull's own. `station` is the station the
/// pull ASKED at — the receipt's `station` — and never its
/// `claim_station`: a pull at a group resolves to a queue in its subtree
/// (`station_resolution: deepest-queue`), so the claim station is the
/// server's answer, not the pull's identity (backlog 9bd65d7c).
fn matches_request(
    receipt: &SelectionReceipt,
    request: uuid::Uuid,
    station: &str,
    actor: &str,
    registered: Option<&str>,
) -> bool {
    receipt["request_id"].as_str() == Some(request.to_string().as_str())
        && receipt["station"]["name"].as_str() == Some(station)
        && names_puller(receipt["actor"].as_str(), actor, registered)
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

    /// A dropped journal frees its actor's lock even while a copy of the
    /// lock's descriptor lives on. A flock belongs to the open file
    /// description, and every fork in this test binary copies every open
    /// descriptor into the child until it execs — the exec-handshake tests
    /// in train/carried_verdict.rs hold children in pre_exec on purpose,
    /// for up to thirty seconds. A journal released by a bare close stayed
    /// locked while such a copy lived, and the dispatch wire test
    /// a_completed_native_run_clears_its_pending_transport_journal, which
    /// drops a journal and opens it again, read "another station pull may
    /// own" its own request (gate-run 4cf59a86, backlog cd70d4b0) — the
    /// same mechanism as a9ae095b in train/entry.rs and 6ff2ab2d in
    /// train/gate_watch.rs. The copy is planted here, so the release is
    /// judged with it alive.
    #[test]
    fn a_dropped_journal_frees_its_lock_while_a_forked_copy_lives() {
        let base = format!("http://journal-release-{}.invalid", uuid::Uuid::new_v4());
        let journal = Journal::open(&base, "agent-codex").unwrap();
        let inherited = journal.lock.try_clone().unwrap();
        drop(journal);
        let reopened = Journal::open(&base, "agent-codex");
        drop(inherited);
        assert!(
            reopened.is_ok(),
            "a dropped journal must not stay locked through a forked copy: {:#}",
            reopened.err().unwrap()
        );
    }

    /// A request that already returned a receipt cannot be absent from the
    /// server; a no-receipt answer for it is a contradiction, never a
    /// reason to forget the claim it recorded (backlog 614c2bb7).
    #[test]
    fn a_no_receipt_answer_never_ends_a_request_that_recorded_one() {
        let base = format!("http://journal-unclaimed-{}.invalid", uuid::Uuid::new_v4());
        let mut journal = Journal::open(&base, "agent-codex").unwrap();
        let pending = journal.request("company").unwrap();
        journal.remember(ORIGINAL).unwrap();
        assert!(journal.unclaimed().is_err());
        drop(journal);
        let mut reopened = Journal::open(&base, "agent-codex").unwrap();
        assert_eq!(
            reopened.request("company").unwrap().request_id,
            pending.request_id,
            "the recorded claim's request is retained"
        );
    }

    /// An answered 4xx is its own typed refusal, so the pull can ask the
    /// receipt door about it; a 5xx (the uncertain commit) is not.
    #[tokio::test]
    async fn station_pull_types_an_answered_refusal_and_not_a_server_error() {
        let http = crate::sor_client::machine_client().unwrap();
        let (base, served) = crate::sor_client::stub::one_response(
            "409 Conflict",
            "station selection refused: selection ingress deadline expired before commit dispatch",
        )
        .await;
        let error = claim_next_at(&http, &base, "company", "agent-codex", None, request_id())
            .await
            .unwrap_err();
        assert!(served.await.unwrap().is_some());
        let refusal = error
            .downcast_ref::<AnsweredRefusal>()
            .expect("an answered 4xx is typed for the receipt door");
        assert!(refusal.to_string().contains("before commit dispatch"));
        let (base, served) = crate::sor_client::stub::one_response(
            "500 Internal Server Error",
            "native selection commit outcome uncertain; retry original request",
        )
        .await;
        let error = claim_next_at(&http, &base, "company", "agent-codex", None, request_id())
            .await
            .unwrap_err();
        assert!(served.await.unwrap().is_some());
        assert!(
            error.downcast_ref::<AnsweredRefusal>().is_none(),
            "an uncertain commit is never asked about, only replayed"
        );
    }

    /// The receipt door's own 404 names the request; a bare 404 is a server
    /// without the door and is never "no receipt" (backlog 614c2bb7).
    #[tokio::test]
    async fn the_receipt_door_is_believed_only_in_its_own_words() {
        let http = crate::sor_client::machine_client().unwrap();
        let own: &'static str = Box::leak(
            serde_json::json!({"request_id": request_id(), "receipt": null})
                .to_string()
                .into_boxed_str(),
        );
        let (base, served) = crate::sor_client::stub::one_response("404 Not Found", own).await;
        let read = receipt_at(&http, &base, "agent-codex", None, request_id()).await;
        let request = served.await.unwrap().unwrap();
        assert!(matches!(read, Ok(ReceiptRead::None)), "{read:?}");
        assert!(request.starts_with(&format!("GET /api/stations/claims/{} ", request_id())));
        for (status, body) in [
            ("404 Not Found", ""),
            // Review a5cd0260: the right id, but no explicit receipt member.
            (
                "404 Not Found",
                "{\"request_id\":\"f679ad30-8400-4440-8675-21e3863c97ea\",\"error\":\"receipt route unavailable\"}",
            ),
            (
                "404 Not Found",
                "{\"request_id\":\"00000000-0000-0000-0000-000000000000\",\"receipt\":null}",
            ),
            (
                "500 Internal Server Error",
                "reading the selection receipt failed",
            ),
            (
                "403 Forbidden",
                "selection receipt belongs to another actor",
            ),
        ] {
            let (base, served) = crate::sor_client::stub::one_response(status, body).await;
            let read = receipt_at(&http, &base, "agent-codex", None, request_id()).await;
            assert!(served.await.unwrap().is_some());
            assert!(
                read.is_err(),
                "{status} {body:?} must keep the request: {read:?}"
            );
        }
        let (base, served) = crate::sor_client::stub::one_response("200 OK", ORIGINAL).await;
        let read = receipt_at(&http, &base, "agent-codex", None, request_id()).await;
        assert!(served.await.unwrap().is_some());
        assert!(matches!(read, Ok(ReceiptRead::Recorded)), "{read:?}");
    }

    #[tokio::test]
    async fn station_pull_keeps_the_original_receipt_bytes_and_its_own_actor() {
        let (base, served) = crate::sor_client::stub::one_response("201 Created", ORIGINAL).await;
        let http = crate::sor_client::machine_client().unwrap();
        let receipt = claim_next_at(&http, &base, "company", "agent-codex", None, request_id())
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
            claim_next_at(&http, &base, "company", "agent-codex", None, request_id())
                .await
                .unwrap()
                .is_none()
        );
        assert!(served.await.unwrap().is_some());
        let (base, served) =
            crate::sor_client::stub::one_response("409 Conflict", "native owner unavailable").await;
        let error = claim_next_at(&http, &base, "company", "agent-codex", None, request_id())
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
            let answer = claim_next_at(&http, &base, station, actor, None, request).await;
            assert!(
                answer.is_err(),
                "a different selection identity is not this request's claim"
            );
            assert!(served.await.unwrap().is_some());
        }
    }

    /// The original of request 2702eadb as the store holds it (backlog
    /// 9bd65d7c), trimmed to the identity it carries: the registered id,
    /// the group station pulled, and the deepest queue it resolved to.
    const ALIAS_ORIGINAL: &str = "{\"version\": 2, \"actor\": \"agent-claude\", \"request_id\": \"f679ad30-8400-4440-8675-21e3863c97ea\", \"station\": {\"name\": \"algedonic\", \"id\": \"algedonic-row\", \"kind\": \"group\"}, \"claim_station\": {\"name\": \"build\", \"id\": \"build-row\"}, \"station_resolution\": \"deepest-queue\", \"selected\": {\"job_id\": \"packet\", \"step_id\": \"step\"}, \"claimed_step\": {\"id\": \"step\", \"job_id\": \"packet\", \"spec_slug\": \"build\", \"status\": \"active\", \"assignee_id\": \"agent-claude\"}}";

    /// An alias's original names its registered id and the station it
    /// pulled, with the queue it resolved to beside it: that is this
    /// pull's claim. A different request, a different station pulled, or
    /// an id the registry does not tie to the alias is still not.
    #[tokio::test]
    async fn station_pull_takes_its_registered_id_and_a_deepest_queue_original() {
        let http = crate::sor_client::machine_client().unwrap();
        let alias = "claude@algedonic.dev";
        let (base, served) =
            crate::sor_client::stub::one_response("201 Created", ALIAS_ORIGINAL).await;
        let receipt = claim_next_at(
            &http,
            &base,
            "algedonic",
            alias,
            Some("agent-claude"),
            request_id(),
        )
        .await;
        assert!(served.await.unwrap().is_some());
        assert_eq!(
            receipt.unwrap().unwrap().json(),
            ALIAS_ORIGINAL,
            "its own original, resolved to a queue under the pulled station"
        );
        for (station, registered, request) in [
            ("algedonic", Some("agent-claude"), uuid::Uuid::nil()),
            ("build", Some("agent-claude"), request_id()),
            ("algedonic", Some("agent-codex"), request_id()),
            ("algedonic", None, request_id()),
        ] {
            let (base, served) =
                crate::sor_client::stub::one_response("201 Created", ALIAS_ORIGINAL).await;
            let answer = claim_next_at(&http, &base, station, alias, registered, request).await;
            assert!(served.await.unwrap().is_some());
            let said = format!("{:#}", answer.unwrap_err());
            assert!(
                said.contains("\"agent-claude\"") && said.contains("\"algedonic\""),
                "the refusal names what the original said: {said}"
            );
        }
        let (base, served) = crate::sor_client::stub::one_response("200 OK", ALIAS_ORIGINAL).await;
        let read = receipt_at(&http, &base, alias, Some("agent-claude"), request_id()).await;
        assert!(served.await.unwrap().is_some());
        assert!(matches!(read, Ok(ReceiptRead::Recorded)), "{read:?}");
        let (base, served) = crate::sor_client::stub::one_response("200 OK", ALIAS_ORIGINAL).await;
        let read = receipt_at(&http, &base, alias, None, request_id()).await;
        assert!(served.await.unwrap().is_some());
        assert!(read.is_err(), "an unregistered alias owns no other id");
    }
}
