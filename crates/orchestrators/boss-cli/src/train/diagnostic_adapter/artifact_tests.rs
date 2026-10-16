use super::*;

#[tokio::test]
async fn complete_large_original_crosses_real_bounded_file_doors_and_replays() {
    let (base, jobs, job, step) = super::tests::owning_records(true).await;
    let root = tempfile::tempdir().unwrap();
    let mut conductor = Conductor::diagnostic_fixture(root.path());
    conductor.cfg.jobs = base;
    let door = NativeDoor {
        conductor: &conductor,
        job,
        step,
        directory: super::super::diagnostic_journal::private_directory(
            &root.path().join("journal"),
        )
        .unwrap(),
        actor: ACTOR.into(),
        content: crate::attach::tests::store().await,
        may_launch: false,
    };
    // Actual retained688 snapshot is 14,053,784 bytes. Exercise that full
    // transport size, including non-ASCII chunk boundaries, through the real
    // content router rather than a fixture that invents an unlimited store.
    let value = json!({"journal":"\u{1f37a}".repeat(3_513_446)});
    assert!(serde_json::to_vec(&value).unwrap().len() >= 14_053_784);
    let serialized = serde_json::to_string(&value).unwrap();
    let mut end = crate::attach::largest_file() as usize;
    while !serialized.is_char_boundary(end) {
        end -= 1;
    }
    // Simulate loss of the acknowledgement after upload but before the
    // immutable part record. The next retain must reconcile these exact bytes.
    let uploaded = door
        .attach_bytes(serialized.as_bytes()[..end].to_vec())
        .await
        .unwrap();
    assert_eq!(door.keep("recovery.journal", &value).await.unwrap(), value);
    let kept = door
        .original_record("recovery.journal")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kept.value["chunks"][0]["file"]["file_id"],
        uploaded["file_id"]
    );
    assert_eq!(
        door.original("recovery.journal").await.unwrap(),
        Some(value.clone())
    );
    let before = jobs.recorded_events().len();
    assert_eq!(door.keep("recovery.journal", &value).await.unwrap(), value);
    assert_eq!(
        jobs.recorded_events().len(),
        before,
        "retry repeated a transition"
    );
    assert!(
        door.keep("recovery.journal", &json!({"changed":true}))
            .await
            .is_err()
    );
    let assessment = json!({"schema":1,"outcome":"recovered-refusal","journal":value});
    assert_eq!(
        door.keep("diagnostic.recovery-assessment", &assessment)
            .await
            .unwrap(),
        assessment
    );
    assert_eq!(
        door.original("diagnostic.recovery-assessment")
            .await
            .unwrap(),
        Some(assessment.clone())
    );
    let manifest = door.artifact(&assessment, None).await.unwrap();
    assert!(manifest["chunks"].as_array().unwrap().len() >= 7);
    let before = jobs.recorded_events().len();
    assert_eq!(door.artifact(&assessment, None).await.unwrap(), manifest);
    assert_eq!(
        jobs.recorded_events().len(),
        before,
        "retained part references changed on retry"
    );
    for fault in 0..14 {
        let mut altered = manifest.clone();
        match fault {
            0 => {
                altered["chunks"].as_array_mut().unwrap().pop();
            }
            1 => {
                let duplicate = altered["chunks"][0].clone();
                altered["chunks"].as_array_mut().unwrap().push(duplicate);
            }
            2 => altered["chunks"][0]["index"] = json!(1),
            3 => altered["chunks"][0]["offset"] = json!(1),
            4 => altered["chunks"][0]["file"]["sha256"] = json!("0".repeat(64)),
            5 => altered["chunks"][0]["file"]["file_id"] = json!(Uuid::new_v4()),
            6 => {
                altered["chunks"][0]["file"]["size_bytes"] =
                    json!(crate::attach::largest_file() + 1)
            }
            7 => altered["chunks"][0]["file"]["encoding"] = json!("binary"),
            8 => altered["size_bytes"] = json!(64 * 1024 * 1024 + 1),
            9 => altered["sha256"] = json!("0".repeat(64)),
            10 => altered["schema"] = json!(2),
            11 => altered["extra"] = json!(true),
            12 => altered["chunks"] = json!(vec![altered["chunks"][0].clone(); 65]),
            _ => {
                let other = altered["chunks"][1]["file"].clone();
                altered["chunks"][0]["file"] = other;
            }
        }
        assert!(door.read_artifact(&altered).await.is_err(), "fault {fault}");
    }
    let invalid = door.attach_bytes(vec![255]).await.unwrap();
    let malformed = json!({"kind":"chunked-json","schema":1,"size_bytes":1,
        "sha256":hex::encode(Sha256::digest([255])),
        "chunks":[{"index":0,"offset":0,"file":invalid}]});
    assert!(
        door.read_artifact(&malformed).await.is_err(),
        "invalid UTF8 was accepted"
    );
    assert_eq!(door.read_artifact(&manifest).await.unwrap(), assessment);
    door.complete_result("recovered-refusal", &assessment)
        .await
        .unwrap();
    let current = door
        .api(
            Method::GET,
            &format!("/api/jobs/{}", door.id().unwrap()),
            None,
        )
        .await
        .unwrap();
    assert!(
        serde_json::to_vec(&current).unwrap().len()
            < boss_events::outbox::MAX_STAGED_EVENT_BYTES / 4
    );
    let evidence = &find_step(&current, "execute").unwrap()["metadata"]["evidence"];
    assert_eq!(door.read_artifact(evidence).await.unwrap(), assessment);
}
