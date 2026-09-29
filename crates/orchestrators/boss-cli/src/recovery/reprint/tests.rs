//! The reprint loop, pinned: the pure rules without a socket, then the
//! whole check against the REAL jobs router (the platform bundle's own
//! protocols, its admission, its step merge door) and the REAL files
//! router, in memory — so what the chore writes is what the API takes.

use std::sync::Arc;

use boss_core::job::{Assurance, JobStatus, SignOffStamp, StepStatus};
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobFilter, JobsRepository, WorkflowRegistry};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use chrono::TimeZone;

use super::*;

const CHORE: &str = "automation:recovery-sheet";
const DAVID: &str = "emp-david";

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, 12, 0, 0).unwrap()
}

fn facts(host: &str) -> Vec<String> {
    let mut f = vec![
        "format recovery-sheet/1".to_string(),
        "01-dev-door/title The dev door".to_string(),
        "01-dev-door/proven_by NOT EXERCISED ON RECORD".to_string(),
        format!("01-dev-door/01 fact Host | infra/estate/estate.toml#dev_host = {host}"),
        "01-dev-door/02 prose Sign in with the passkey.".to_string(),
        "02-web/title The web app".to_string(),
        "02-web/needs/01 the gateway up".to_string(),
    ];
    f.sort();
    f
}

fn tree(host: &str) -> Tree {
    let facts = facts(host);
    Tree {
        fact_hash: hash_of(&facts),
        facts,
        origin_sha: format!("sha-for-{host}"),
        rendered_at: "2026-10-01 12:00 UTC".into(),
    }
}

fn pdf_for(t: &Tree) -> impl FnOnce() -> Result<Pdf> {
    let bytes = format!("%PDF-1.4 the sheet at {}\n", t.fact_hash).into_bytes();
    move || {
        Ok(Pdf {
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
        })
    }
}

fn no_print() -> Result<Pdf> {
    Err(anyhow!("nothing is owed, so nothing is printed"))
}

// ----- the pure rules --------------------------------------------------

#[test]
fn the_diff_names_each_moved_fact_by_its_source_and_ignores_positions() {
    let old = facts("dev.example.test");
    let mut new = facts("door.example.test");
    // A prose line inserted ahead of the fact shifts its position; the
    // diff is one `+`, not a cascade of moved lines.
    new.push("01-dev-door/01 prose Open a terminal first.".to_string());
    new.iter_mut()
        .filter(|l| l.starts_with("01-dev-door/01 fact"))
        .for_each(|l| *l = l.replacen("01-dev-door/01", "01-dev-door/02", 1));
    new.iter_mut()
        .filter(|l| l.starts_with("01-dev-door/02 prose Sign"))
        .for_each(|l| *l = l.replacen("01-dev-door/02", "01-dev-door/03", 1));
    new.sort();
    assert_eq!(
        diff(&old, &new),
        [
            "dev-door: Host | infra/estate/estate.toml#dev_host: dev.example.test -> door.example.test",
            "dev-door: + prose Open a terminal first.",
        ]
    );
    assert!(diff(&old, &old).is_empty());
}

#[test]
fn a_road_removed_is_listed_line_by_line() {
    let old = facts("h");
    let new: Vec<String> = old
        .iter()
        .filter(|l| !l.starts_with("02-web"))
        .cloned()
        .collect();
    assert_eq!(
        diff(&old, &new),
        ["web: - needs the gateway up", "web: - title The web app"]
    );
}

fn step_json(slug: &str, status: &str, md: Value, stamps: Vec<SignOffStamp>) -> Value {
    let mut step = boss_core::job::Step::new(
        boss_core::job::JobId::new(),
        "sign-off",
        "Print the recovery sheet and sign for the copy it replaces",
        2,
    );
    step.spec_slug = Some(slug.to_string());
    step.metadata = md;
    step.status = serde_json::from_value(json!(status)).unwrap();
    if step.status == StepStatus::Completed {
        step.completed_at = Some(at(2));
    }
    let shape = step.shape_hash();
    step.sign_offs = stamps
        .into_iter()
        .map(|mut s| {
            s.shape_hash = shape.clone();
            s
        })
        .collect();
    serde_json::to_value(step).unwrap()
}

fn stamp(assurance: Assurance) -> SignOffStamp {
    SignOffStamp {
        authority_id: DAVID.into(),
        role: "platform-admin".into(),
        stamped_at: at(2),
        shape_hash: String::new(),
        assurance,
        presence_nonce: None,
        voided_at: None,
        voided_by_event: None,
    }
}

fn packet(id: &str, status: &str, md: Value, step: Value) -> Value {
    json!({"id": id, "status": status, "metadata": md, "steps": [step]})
}

/// What is on paper is a SIGNATURE: completed, approved, and a live
/// PRESENCE stamp. A session stamp, an unapproved decision or an open
/// step is not the paper, however it reads.
#[test]
fn only_a_presence_signed_approval_is_what_is_on_paper() {
    let f = facts("h");
    let h = hash_of(&f);
    let md = json!({"fact_hash": h, "decision": "approved"});
    let signed = packet(
        "p-signed",
        "closed",
        json!({"facts": f}),
        step_json(
            PRINT_STEP,
            "completed",
            md.clone(),
            vec![stamp(Assurance::Presence)],
        ),
    );
    let record = read_record(std::slice::from_ref(&signed), &[]).unwrap();
    let paper = record.on_paper.expect("a signed print is on paper");
    assert_eq!(paper.fact_hash, h);
    assert_eq!(paper.facts.as_deref(), Some(f.as_slice()));

    for (why, row) in [
        (
            "a session stamp",
            packet(
                "p1",
                "closed",
                json!({}),
                step_json(
                    PRINT_STEP,
                    "completed",
                    md.clone(),
                    vec![stamp(Assurance::Session)],
                ),
            ),
        ),
        (
            "rejected",
            packet(
                "p2",
                "closed",
                json!({}),
                step_json(
                    PRINT_STEP,
                    "completed",
                    json!({"fact_hash": h, "decision": "rejected"}),
                    vec![stamp(Assurance::Presence)],
                ),
            ),
        ),
    ] {
        let r = read_record(&[row], &[]).unwrap();
        assert!(r.on_paper.is_none(), "{why} is not on paper");
    }

    // A fact set that does not hash to the signed version is not used
    // as the old side of a diff.
    let mut md2 = signed.clone();
    md2["metadata"]["facts"] = json!(["something else"]);
    let r = read_record(&[md2], &[]).unwrap();
    assert!(r.on_paper.unwrap().facts.is_none());
}

/// A print packet signed on `day` for `hash`: completed, approved, one
/// live presence stamp.
fn signed_packet(id: &str, hash: &str, day: u32) -> Value {
    let mut row = packet(
        id,
        "closed",
        json!({}),
        step_json(
            PRINT_STEP,
            "completed",
            json!({"fact_hash": hash, "decision": "approved"}),
            vec![stamp(Assurance::Presence)],
        ),
    );
    row["steps"][0]["completed_at"] = json!(at(day));
    row
}

/// Review of car 3 (run ff8ffd04), mutations M1 and M3: a stamp that
/// does not attest the step AS IT STANDS — its shape moved on, or it
/// was voided — is not a signature, and an approved presence stamp on
/// a step that never completed is not the paper either.
#[test]
fn a_stale_or_voided_stamp_or_an_open_step_is_not_the_paper() {
    let live = signed_packet("p-live", "aaaa", 2);
    assert!(
        read_record(std::slice::from_ref(&live), &[])
            .unwrap()
            .on_paper
            .is_some(),
        "control: the live signature is the paper"
    );

    let mut stale = live.clone();
    stale["steps"][0]["sign_offs"][0]["shape_hash"] = json!("the shape before an edit");
    let mut voided = live.clone();
    voided["steps"][0]["sign_offs"][0]["voided_at"] = json!(at(3));
    let open = packet(
        "p-open",
        "open",
        json!({}),
        step_json(
            PRINT_STEP,
            "ready",
            json!({"fact_hash": "aaaa", "decision": "approved"}),
            vec![stamp(Assurance::Presence)],
        ),
    );
    // Not completed, though it carries a completion instant: a skipped
    // step, and an open one with a stale `completed_at` — the step's
    // STATUS decides, never the presence of a timestamp.
    let mut skipped = live.clone();
    skipped["steps"][0]["status"] = json!("skipped");
    let mut reopened = open.clone();
    reopened["steps"][0]["completed_at"] = json!(at(2));
    for (why, row) in [
        ("a stamp on another shape", stale),
        ("a voided stamp", voided),
        ("an open step", open),
        ("a skipped step", skipped),
        ("a ready step carrying a completion instant", reopened),
    ] {
        let r = read_record(&[row], &[]).unwrap();
        assert!(r.on_paper.is_none(), "{why} is not on paper");
    }
}

/// Mutation M4: two signed packets resolve to the NEWEST signature,
/// whatever order the list serves them in — the older one read as the
/// paper would re-file print jobs for ever.
#[test]
fn two_signed_print_jobs_resolve_to_the_newest() {
    let older = signed_packet("p-older", "aaaa", 2);
    let newer = signed_packet("p-newer", "bbbb", 5);
    for rows in [
        vec![older.clone(), newer.clone()],
        vec![newer.clone(), older.clone()],
    ] {
        let paper = read_record(&rows, &[]).unwrap().on_paper.unwrap();
        assert_eq!(paper.packet, "p-newer");
        assert_eq!(paper.fact_hash, "bbbb");
        assert_eq!(paper.signed_at, at(5));
    }
}

/// Mutation M7: an open print job is WHOLE only when its step names an
/// attached PDF whose sha256 is the one the packet names. A step and a
/// packet that disagree about the bytes is not left pending: it is
/// refreshed, so the two are rewritten from one render.
#[test]
fn a_print_job_whose_step_and_packet_disagree_on_the_pdf_is_refreshed() {
    let open = |packet_sha: &str| {
        packet(
            "p-open",
            "open",
            json!({"pdf_sha256": packet_sha}),
            step_json(
                PRINT_STEP,
                "ready",
                json!({"fact_hash": "bbbb", "pdf_file_ref": "f-1", "pdf_sha256": "sha-a"}),
                vec![],
            ),
        )
    };
    let paper = signed_packet("p-paper", "aaaa", 2);
    let agree = read_record(&[paper.clone(), open("sha-a")], &[]).unwrap();
    assert!(agree.open_print.as_ref().unwrap().whole);
    assert_eq!(plan(&agree, "bbbb", at(9)).0, Reprint::Pending, "control");
    let disagree = read_record(&[paper, open("sha-b")], &[]).unwrap();
    assert!(!disagree.open_print.as_ref().unwrap().whole);
    assert!(matches!(
        plan(&disagree, "bbbb", at(9)).0,
        Reprint::Refresh { ref why } if why.contains("not attached as the packet names it")
    ));
}

/// Mutation M8: a write the API answered 204 to and did not keep is
/// refused, naming each key that did not land and where.
#[test]
fn a_write_that_did_not_land_is_refused_by_name() {
    let md: Map<String, Value> = [
        ("fact_hash".to_string(), json!("bbbb")),
        ("context_md".to_string(), json!("case")),
    ]
    .into_iter()
    .collect();
    let signed: Map<String, Value> = [
        ("fact_hash".to_string(), json!("bbbb")),
        ("pdf_file_ref".to_string(), json!("f-2")),
    ]
    .into_iter()
    .collect();
    let read_back = |job_md: Value, step_md: Value| {
        packet(
            "p-1",
            "open",
            job_md,
            step_json(PRINT_STEP, "ready", step_md, vec![]),
        )
    };
    let whole = read_back(
        json!({"fact_hash": "bbbb", "context_md": "case"}),
        json!({"fact_hash": "bbbb", "pdf_file_ref": "f-2"}),
    );
    confirm_written("p-1", &whole, &md, &signed).expect("control: everything landed");

    let step_lost = read_back(
        json!({"fact_hash": "bbbb", "context_md": "case"}),
        json!({"fact_hash": "aaaa", "pdf_file_ref": "f-1"}),
    );
    let err = confirm_written("p-1", &step_lost, &md, &signed)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("on its `print` step: [fact_hash, pdf_file_ref]"),
        "{err}"
    );
    let packet_lost = read_back(
        json!({"fact_hash": "bbbb"}),
        json!({"fact_hash": "bbbb", "pdf_file_ref": "f-2"}),
    );
    let err = confirm_written("p-1", &packet_lost, &md, &signed)
        .unwrap_err()
        .to_string();
    assert!(err.contains("on the packet: [context_md]"), "{err}");
}

#[test]
fn two_open_print_jobs_are_refused_rather_than_guessed_between() {
    let open = |id: &str| {
        packet(
            id,
            "open",
            json!({}),
            step_json(PRINT_STEP, "ready", json!({"fact_hash": "x"}), vec![]),
        )
    };
    let err = read_record(&[open("p-a"), open("p-b")], &[]).unwrap_err();
    assert!(err.to_string().contains("p-a, p-b"), "{err}");
}

fn paper(hash: &str, day: u32) -> OnPaper {
    OnPaper {
        fact_hash: hash.into(),
        packet: "paper-packet-1".into(),
        signed_at: at(day),
        facts: None,
    }
}

#[test]
fn the_plan_follows_the_record() {
    let now = at(10);
    // Bootstrap: nothing signed, nothing open — the first print job.
    let (r, e) = plan(&Record::default(), "aaaa", now);
    assert!(matches!(r, Reprint::File { ref why } if why.contains("first print job")));
    assert_eq!(e, Existence::NotApplicable);

    // The paper says what the tree says.
    let current = Record {
        on_paper: Some(paper("aaaa", 1)),
        ..Record::default()
    };
    let (r, e) = plan(&current, "aaaa", now);
    assert_eq!(r, Reprint::Current { stray: None });
    assert_eq!(
        e,
        Existence::NotDue(at(1) + Duration::days(EXISTENCE_CHECK_DAYS))
    );
    let (_, e) = plan(
        &current,
        "aaaa",
        at(1) + Duration::days(EXISTENCE_CHECK_DAYS),
    );
    assert_eq!(
        e,
        Existence::Due(at(1) + Duration::days(EXISTENCE_CHECK_DAYS))
    );

    // A `kept` confirmation restarts the clock; an open check is not
    // filed twice.
    let kept = Record {
        last_kept_at: Some(at(5)),
        ..current.clone()
    };
    assert_eq!(
        plan(&kept, "aaaa", now).1,
        Existence::NotDue(at(5) + Duration::days(EXISTENCE_CHECK_DAYS))
    );
    let asked = Record {
        open_confirmation: Some("check-1".into()),
        ..current.clone()
    };
    assert_eq!(
        plan(&asked, "aaaa", now).1,
        Existence::Open("check-1".into())
    );

    // The tree moved: a print job is owed, and no existence check.
    let (r, e) = plan(&current, "bbbb", now);
    assert!(matches!(r, Reprint::File { ref why } if why.contains("tree says version bbbb")));
    assert_eq!(e, Existence::NotApplicable);

    // One is open for another version: refresh it. For this one, whole:
    // pending. For this one, but its PDF not attached: refresh.
    let open = |hash: &str, whole: bool| Record {
        open_print: Some(OpenPrint {
            packet: "print-1".into(),
            print_step: "s".into(),
            fact_hash: hash.into(),
            whole,
            metadata: json!({}),
        }),
        ..current.clone()
    };
    assert!(matches!(
        plan(&open("cccc", true), "bbbb", now).0,
        Reprint::Refresh { .. }
    ));
    assert_eq!(plan(&open("bbbb", true), "bbbb", now).0, Reprint::Pending);
    assert!(matches!(
        plan(&open("bbbb", false), "bbbb", now).0,
        Reprint::Refresh { ref why } if why.contains("not attached")
    ));
    // Current, with a print job still open for a version the tree no
    // longer says: named, so it is declined rather than printed.
    assert_eq!(
        plan(&open("cccc", true), "aaaa", now).0,
        Reprint::Current {
            stray: Some("print-1".into())
        }
    );

    // The copy on paper reported missing after it was signed: reprint
    // the same version.
    let missing = Record {
        last_confirmation: Some(Confirmation {
            packet: "check-2".into(),
            at: at(6),
            kept: false,
            fact_hash: "aaaa".into(),
        }),
        ..current.clone()
    };
    let (r, _) = plan(&missing, "aaaa", now);
    assert!(matches!(r, Reprint::File { ref why } if why.contains("reported missing")));
    assert!(statement("aaaa", &missing).contains("reported missing"));
}

#[test]
fn the_statement_names_both_versions_and_the_packet_that_signed_the_old() {
    let record = Record {
        on_paper: Some(paper(&"a".repeat(64), 1)),
        ..Record::default()
    };
    let s = statement(&"b".repeat(64), &record);
    assert!(
        s.contains(&format!(
            "version {}",
            "b".repeat(crate::recovery::VERSION_CHARS)
        )),
        "{s}"
    );
    assert!(
        s.contains(&format!(
            "destroyed the copy of version {}",
            "a".repeat(crate::recovery::VERSION_CHARS)
        )),
        "{s}"
    );
    assert!(s.contains("packet paper-pa"), "{s}");
    assert!(statement("x", &Record::default()).contains("No earlier printed version"));
}

// ----- the whole check, against the real routers -----------------------

async fn serve_jobs() -> (String, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> =
        Arc::new(FakePolicyClient::builder().with_default_rules().build());
    let bus = boss_testing::RecordingEventBus::new();
    let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
    let publisher = boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs");
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let state = boss_jobs::http::JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let app = boss_jobs::http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), jobs)
}

fn chore(base: &str) -> Wire {
    Wire::at(
        base.to_string(),
        Some(identity::Caller {
            id: CHORE.into(),
            source: identity::Source::Env,
        }),
    )
    .unwrap()
}

async fn of_kind(jobs: &InMemoryJobs, kind: &str) -> Vec<boss_core::job::Job> {
    let mut rows = jobs
        .list_jobs(&JobFilter::default(), 100, 0)
        .await
        .unwrap()
        .0;
    rows.retain(|j| j.kind == kind);
    rows
}

async fn print_step(jobs: &InMemoryJobs, job: &boss_core::job::Job) -> boss_core::job::Step {
    jobs.list_steps(&job.id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some(PRINT_STEP))
        .expect("the print job has a print step")
}

/// David's act, done the way the API records it: the decision saved, a
/// presence stamp over the step's shape AS IT STANDS, the step completed
/// and the packet closed on `printed`.
async fn david_signs(jobs: &InMemoryJobs, job: &boss_core::job::Job, when: DateTime<Utc>) {
    let id = print_step(jobs, job).await.id;
    let decision = serde_json::Map::from_iter([("decision".to_string(), json!("approved"))]);
    let es = boss_core::publisher::EventStamp::new("jobs", boss_core::actor::ActorId::human(DAVID));
    jobs.merge_step_metadata_at(&id, &decision, &es)
        .await
        .unwrap();
    let decided = jobs.get_step(&id).await.unwrap().unwrap();
    let signature = SignOffStamp {
        shape_hash: decided.shape_hash(),
        presence_nonce: Some("nonce".into()),
        stamped_at: when,
        ..stamp(Assurance::Presence)
    };
    jobs.append_sign_off(&id, &signature, &es, &[])
        .await
        .unwrap();
    let (mut step, read) = jobs.get_step_versioned(&id).await.unwrap().unwrap();
    assert_eq!(step.live_stamps().count(), 1, "the signature is live");
    step.status = StepStatus::Completed;
    step.completed_at = Some(when);
    jobs.update_step_if_unchanged_at(&step, read, when, &[])
        .await
        .unwrap();
    let mut closed = jobs.get_job(&job.id).await.unwrap().unwrap();
    closed.status = JobStatus::Closed;
    jobs.update_job(&closed).await.unwrap();
}

/// THE LOOP, end to end: the first run files print job #1 with the PDF
/// on its `print` step; a second run on the same tree writes nothing; a
/// moved fact refreshes the SAME packet in place; David signs; the
/// paper is current; ninety days on the existence check files itself;
/// and the next moved fact files print job #2, whose diff names the old
/// value and the new one and whose statement names the copy it replaces.
#[tokio::test(flavor = "multi_thread")]
async fn the_reprint_loop_files_refreshes_and_reads_the_signature_back() {
    let (base, jobs) = serve_jobs().await;
    let files = crate::attach::tests::store().await;
    let wire = chore(&base);

    // 1. Bootstrap: nothing on paper, the first print job files itself.
    let t1 = tree("dev.example.test");
    let out = check(&wire, &files, &t1, pdf_for(&t1), at(1))
        .await
        .unwrap();
    let verdict = out.iter().find(|l| l.starts_with("verdict: ")).unwrap();
    assert!(verdict.contains("print job filed"), "{out:#?}");
    assert!(
        out.iter().any(|l| l == "on paper nothing on record"),
        "{out:#?}"
    );
    let filed = of_kind(&jobs, REPRINT_KIND).await;
    assert_eq!(filed.len(), 1, "one print job");
    let job = &filed[0];
    let step = print_step(&jobs, job).await;
    assert_eq!(step.metadata["fact_hash"], json!(t1.fact_hash));
    assert_eq!(step.metadata["replaces_version"], json!("none on record"));
    assert!(
        step.metadata["statement"]
            .as_str()
            .unwrap()
            .contains("No earlier printed version"),
        "{}",
        step.metadata
    );
    let file_ref = step.metadata["pdf_file_ref"].as_str().unwrap().to_string();
    let pdf1 = pdf_for(&t1)().unwrap();
    assert_eq!(step.metadata["pdf_sha256"], json!(pdf1.sha256));
    // The PDF is on the print step, under a name carrying the version.
    let listed: Value = reqwest::Client::new()
        .get(format!(
            "{files}/api/files?target_kind=step&target_id={}",
            step.id
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["id"], json!(file_ref));
    assert_eq!(
        listed[0]["filename"],
        json!(format!("recovery-sheet-{}.pdf", version_of(&t1.fact_hash)))
    );

    // 2. The same tree again: pending, nothing written, nothing printed.
    let out = check(&wire, &files, &t1, no_print, at(1)).await.unwrap();
    assert!(
        out.iter()
            .any(|l| l.starts_with("verdict: print job pending")),
        "{out:#?}"
    );
    assert_eq!(of_kind(&jobs, REPRINT_KIND).await.len(), 1);

    // 3. A fact moves before David prints: the SAME packet, refreshed.
    let t2 = tree("door.example.test");
    let out = check(&wire, &files, &t2, pdf_for(&t2), at(2))
        .await
        .unwrap();
    assert!(
        out.iter()
            .any(|l| l.starts_with("verdict: print job refreshed")),
        "{out:#?}"
    );
    let all = of_kind(&jobs, REPRINT_KIND).await;
    assert_eq!(all.len(), 1, "refreshed in place, never a pile");
    let job = jobs.get_job(&all[0].id).await.unwrap().unwrap();
    let step = print_step(&jobs, &job).await;
    assert_eq!(step.metadata["fact_hash"], json!(t2.fact_hash));
    let superseded = job.metadata["superseded_renders"].as_array().unwrap();
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0]["fact_hash"], json!(t1.fact_hash));
    assert_eq!(superseded[0]["pdf_file_ref"], json!(file_ref));
    // ONE PDF on the step after the refresh (review of car 3, finding
    // 4): the replaced render is detached, so the download list cannot
    // hand David the page his signature does not name.
    let listed: Value = reqwest::Client::new()
        .get(format!(
            "{files}/api/files?target_kind=step&target_id={}",
            step.id
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1, "exactly one PDF on the step: {listed:?}");
    assert_eq!(listed[0]["id"], step.metadata["pdf_file_ref"]);
    assert_ne!(listed[0]["id"], json!(file_ref), "the replaced one is gone");
    assert_eq!(
        listed[0]["filename"],
        json!(format!("recovery-sheet-{}.pdf", version_of(&t2.fact_hash)))
    );

    // 4. David prints version 2 and signs. The paper is now version 2.
    david_signs(&jobs, &job, at(3)).await;
    let out = check(&wire, &files, &t2, no_print, at(4)).await.unwrap();
    assert!(
        out.iter().any(|l| l.starts_with("verdict: current")),
        "{out:#?}"
    );
    assert!(
        out.iter()
            .any(|l| l.starts_with("existence: next check due 2027-01-01")),
        "{out:#?}"
    );

    // 5. Ninety days on, the existence check files itself — once.
    let later = at(3) + Duration::days(EXISTENCE_CHECK_DAYS);
    let out = check(&wire, &files, &t2, no_print, later).await.unwrap();
    assert!(
        out.iter()
            .any(|l| l.starts_with("verdict: existence check filed")),
        "{out:#?}"
    );
    let checks = of_kind(&jobs, KEPT_KIND).await;
    assert_eq!(checks.len(), 1);
    let confirm = jobs
        .list_steps(&checks[0].id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some(CONFIRM_STEP))
        .unwrap();
    assert_eq!(confirm.metadata["fact_hash"], json!(t2.fact_hash));
    assert_eq!(confirm.metadata["print_packet"], json!(job.id.to_string()));
    let out = check(&wire, &files, &t2, no_print, later).await.unwrap();
    assert!(
        out.iter().any(|l| l.starts_with("existence: check open")),
        "{out:#?}"
    );
    assert_eq!(of_kind(&jobs, KEPT_KIND).await.len(), 1, "never twice");

    // 6. The next change files print job #2, whose diff and statement
    //    are read off the signed packet.
    let t3 = tree("gate.example.test");
    let out = check(&wire, &files, &t3, pdf_for(&t3), later)
        .await
        .unwrap();
    assert!(
        out.iter()
            .any(|l| l.starts_with("verdict: print job filed")),
        "{out:#?}"
    );
    assert!(
        out.iter().any(|l| l.contains(
            "dev-door: Host | infra/estate/estate.toml#dev_host: door.example.test -> gate.example.test"
        )),
        "{out:#?}"
    );
    let open: Vec<_> = of_kind(&jobs, REPRINT_KIND)
        .await
        .into_iter()
        .filter(|j| j.status == JobStatus::Open)
        .collect();
    assert_eq!(open.len(), 1);
    let s = print_step(&jobs, &open[0]).await;
    let statement = s.metadata["statement"].as_str().unwrap();
    assert!(
        statement.contains(&format!(
            "destroyed the copy of version {}",
            version_of(&t2.fact_hash)
        )),
        "{statement}"
    );
    assert_eq!(
        s.metadata["replaces_version"],
        json!(version_of(&t2.fact_hash))
    );
}

/// Mutation M5: the sha256 the passkey will sign is the sha256 of the
/// bytes the store holds. A print whose stated digest is not its bytes'
/// is refused before any key names it, and the half-filed packet it
/// leaves is not WHOLE, so the next run refreshes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_pdf_whose_digest_is_not_its_bytes_is_refused() {
    let (base, _jobs) = serve_jobs().await;
    let files = crate::attach::tests::store().await;
    let wire = chore(&base);
    let t = tree("dev.example.test");
    let lying = || {
        Ok(Pdf {
            bytes: b"%PDF-1.4 the real bytes\n".to_vec(),
            sha256: "0".repeat(64),
        })
    };
    let err = check(&wire, &files, &t, lying, at(1))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("the PDF printed is"), "{err}");
    let out = check(&wire, &files, &t, pdf_for(&t), at(1)).await.unwrap();
    assert!(
        out.iter()
            .any(|l| l.starts_with("verdict: print job refreshed") && l.contains("not attached")),
        "{out:#?}"
    );
}

/// The keys this verb writes for the passkey to sign are exactly the
/// `print` step's filer fields in the bundled protocol, each bound from
/// the packet — so a rename on either side fails here, not as a print
/// job the signing surface asks David to type into.
#[test]
fn the_signed_keys_are_the_print_steps_filer_fields() {
    let specs = seedable_platform_workflows();
    let spec = |kind: &str| {
        specs
            .iter()
            .find(|w| w.kind == kind)
            .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
    };
    let print = spec(REPRINT_KIND)
        .steps
        .iter()
        .find(|s| s.title == PRINT_STEP)
        .expect("the print job has a print step");
    let filer: BTreeSet<&str> = print
        .fields
        .iter()
        .filter(|f| f.filled_by == boss_core::job::FilledBy::Filer)
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(filer, SIGNED.iter().copied().collect::<BTreeSet<_>>());
    for key in SIGNED {
        assert_eq!(
            print.metadata_defaults.get(key).and_then(Value::as_str),
            Some(format!("{{metadata.{key}}}").as_str()),
            "`{key}` is bound into the signed step from the packet"
        );
    }
    assert!(
        spec(KEPT_KIND)
            .steps
            .iter()
            .any(|s| s.title == CONFIRM_STEP && s.kind == "sign-off"),
        "the existence check is a sign-off named {CONFIRM_STEP}"
    );
}
