//! The separate boss-gcp runner receiver refuses malformed transport before
//! replacing the host's working credential. No recovery receiver is reused.
use boss_testing::{feed_stdin, repo_root, scratch_dir, write_exec, write_file};
use std::io::Write;
use std::process::{Command, Stdio};

fn fixture_base64(text: &str) -> String {
    let mut child = Command::new("base64")
        .arg("-w0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, text.as_bytes());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn sender_conserves_bound_input_and_original_receipt_through_pinned_transport() {
    use std::io::{BufRead, BufReader};
    for (foreign, enrollment_id) in [
        (false, Some("00000000-0000-4000-8000-000000000001")),
        (true, Some("00000000-0000-4000-8000-000000000001")),
        (false, Some("00000000-0000-4000-8000-000000000099")),
        (false, None),
    ] {
        let root = scratch_dir("sender-original-receipt");
        let key = root.join("key");
        assert!(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .output()
                .unwrap()
                .status
                .success()
        );
        let public = Command::new("ssh-keygen")
            .args(["-y", "-f"])
            .arg(&key)
            .output()
            .unwrap();
        assert!(public.status.success());
        let public = String::from_utf8(public.stdout).unwrap().trim().to_string();
        let job = "00000000-0000-4000-8000-000000000001";
        let attempt = "00000000-0000-4000-8000-000000000002";
        let uid = "00000000-0000-4000-8000-000000000003";
        let value = "fake-sensitive-input-000000000000000000";
        let credential = "ops-runner-credential-boss-gcp";
        let witness = serde_json::json!({"version":1,"host":"boss-gcp","credential_id":credential,"job_id":job,"attempt":attempt,"uid":uid,"promoted":false});
        let secret = serde_json::json!({"metadata":{"uid":uid},"data":{"boss-gcp.next":fixture_base64(value),"boss-gcp.next.minted-for":fixture_base64(job),format!("boss-gcp.recovery.{job}.witness"):fixture_base64(&witness.to_string())}});
        let receipt = serde_json::json!({"version":1,"recorded":true,"host":"boss-gcp","credential_id":credential,"job_id":job,"attempt":attempt,"secret_uid":if foreign { "foreign-uid" } else { uid },"original_delivery_receipt":{"version":1,"credential_id":credential,"phase":"verified","actor":"automation:ops-runner","observation_id":format!("{attempt}:delivered"),"event_id":"original-event","timestamp":"original-time","evidence_json":serde_json::json!({"purpose":"authenticated-runner-delivery","job_id":job,"host":"boss-gcp","attempt":attempt}).to_string()}});
        write_file(&root.join("secret"), &secret.to_string());
        write_file(&root.join("receipt"), &receipt.to_string());
        let known = root.join("known_hosts");
        write_file(&known, "fixture-pinned-host-key");
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        write_exec(
            &bin.join("kubectl"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$FIXTURE_ROOT/kubectl-args\"\ncat \"$FIXTURE_ROOT/secret\"\n",
        );
        write_exec(
            &bin.join("ssh"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$FIXTURE_ROOT/ssh-args\"\ncat > \"$FIXTURE_ROOT/wire\"\ncat \"$FIXTURE_ROOT/receipt\"\n",
        );
        let mut packet = serde_json::json!({"kind":"prepare-a-deposit-key","subject":{"id":"runner-deposit-key"},"steps":[{"spec_slug":"enroll","status":"completed","metadata":{"public_key":public,"receiver_host":"boss-gcp","purpose":"ops-runner credential deposit","decision":"approved","enrollment_record":"exact-purpose-install-record"}}]});
        if let Some(id) = enrollment_id {
            packet["id"] = serde_json::json!(id);
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            let body = packet.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let estate: toml::Value = toml::from_str(
            &std::fs::read_to_string(repo_root().join("infra/estate/estate.toml")).unwrap(),
        )
        .unwrap();
        let host = estate["node"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"].as_str() == Some("boss-gcp"))
            .unwrap()["address"]
            .as_str()
            .unwrap();
        let output = Command::new("bash")
            .arg(repo_root().join("infra/gcp/runner-credential-send.sh"))
            .env("BOSS_RUNNER_DEPOSIT_ENROLLMENT", job)
            .env("BOSS_RUNNER_DEPOSIT_KEY", &key)
            .env("BOSS_RUNNER_DEPOSIT_KNOWN_HOSTS", &known)
            .env("BOSS_RUNNER_DEPOSIT_TARGET", format!("fixture@{host}"))
            .env("BOSS_JOBS_URL", format!("http://{address}"))
            .env("BOSS_MACHINE_TOKEN_DIR", root.join("absent-token"))
            .env("FIXTURE_ROOT", &root)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        server.join().unwrap();
        if enrollment_id != Some(job) {
            assert_eq!(
                output.status.code(),
                Some(78),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!root.join("kubectl-args").exists());
            assert!(!root.join("ssh-args").exists());
            continue;
        }
        assert_eq!(
            output.status.code(),
            Some(if foreign { 1 } else { 0 }),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let wire: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("wire")).unwrap()).unwrap();
        assert_eq!(wire["value"], value);
        assert_eq!(wire["job_id"], job);
        assert_eq!(wire["attempt"], attempt);
        assert_eq!(wire["secret_uid"], uid);
        assert_eq!(
            std::fs::read_to_string(root.join("kubectl-args"))
                .unwrap()
                .trim(),
            "-n boss get secret ops-runner-credential -o json"
        );
        let args = std::fs::read_to_string(root.join("ssh-args")).unwrap();
        for flag in [
            "-F /dev/null",
            "StrictHostKeyChecking=yes",
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "GlobalKnownHostsFile=/dev/null",
        ] {
            assert!(args.contains(flag), "{flag}");
        }
        assert!(args.contains(&format!("UserKnownHostsFile={}", known.display())));
        assert!(!args.contains(value));
        if !foreign {
            let returned: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(returned, receipt);
        }
        assert!(!String::from_utf8_lossy(&output.stdout).contains(value));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(value));
    }
}

#[test]
fn inert_declaration_names_the_existing_issuer_and_separate_enrollment() {
    let declaration: toml::Value = toml::from_str(
        &std::fs::read_to_string(repo_root().join("infra/gcp/runner-credential-declaration.toml"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        declaration["credential"]["id"].as_str(),
        Some("ops-runner-credential-boss-gcp")
    );
    assert_eq!(declaration["credential"]["host"].as_str(), Some("boss-gcp"));
    assert_eq!(
        declaration["credential"]["secret_namespace"].as_str(),
        Some("boss")
    );
    assert_eq!(
        declaration["credential"]["secret_name"].as_str(),
        Some("ops-runner-credential")
    );
    assert_eq!(
        declaration["credential"]["issuer_handler"].as_str(),
        Some("credential.rotate.ops-runner")
    );
    assert_eq!(
        declaration["transport"]["credential_id"].as_str(),
        Some("runner-deposit-key")
    );
    assert_eq!(
        declaration["transport"]["protocol_kind"].as_str(),
        Some("prepare-a-deposit-key")
    );
    assert_eq!(
        declaration["activation"]["automatic"].as_bool(),
        Some(false)
    );
    let rules = declaration.get("rule").and_then(toml::Value::as_array);
    assert_eq!(
        rules.map(Vec::len),
        Some(2),
        "stage and promotion declarations are both owed"
    );
    let rules = rules.unwrap();
    assert_eq!(rules[0]["do"][0]["args"], rules[1]["do"][0]["args"]);
    assert_eq!(
        rules[0]["on_event"].as_str(),
        Some("step.done.credential-rotation")
    );
    assert_eq!(
        rules[1]["on_event"].as_str(),
        Some("step.done.credential-delivery")
    );
    for path in [
        "infra/gcp/runner-credential-recv.sh",
        "infra/gcp/runner-credential-send.sh",
    ] {
        let body = std::fs::read_to_string(repo_root().join(path)).unwrap();
        assert!(body.contains(declaration["credential"]["id"].as_str().unwrap()));
    }
}

#[test]
fn sender_without_recorded_enrollment_refuses_before_transport() {
    let root = scratch_dir("runner-sender-no-enrollment");
    let output = Command::new("bash")
        .arg(repo_root().join("infra/gcp/runner-credential-send.sh"))
        .env("BOSS_JOBS_URL", "http://127.0.0.1:1")
        .env("BOSS_RUNNER_DEPOSIT_KEY", root.join("absent-key"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&output.stderr).contains("enrollment"));
}

#[test]
fn sender_rejects_mismatched_recorded_enrollment_before_secret_or_ssh() {
    use std::io::{BufRead, BufReader};
    for field in [
        "receiver_host",
        "purpose",
        "public_key",
        "decision",
        "enrollment_record",
    ] {
        let root = scratch_dir(&format!("sender-enrollment-{field}"));
        let key = root.join("fixture-key");
        let generated = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .output()
            .unwrap();
        assert!(generated.status.success());
        let public = Command::new("ssh-keygen")
            .args(["-y", "-f"])
            .arg(&key)
            .output()
            .unwrap();
        assert!(public.status.success());
        let public = String::from_utf8(public.stdout).unwrap().trim().to_string();
        let known = root.join("known_hosts");
        write_file(&known, "fixture-host-pin");
        let mut metadata = serde_json::json!({"public_key":public,"receiver_host":"boss-gcp","purpose":"ops-runner credential deposit","decision":"approved","enrollment_record":"recorded-exact-receiver"});
        metadata[field] = serde_json::json!(if field == "enrollment_record" {
            ""
        } else {
            "mismatch"
        });
        let packet = serde_json::json!({"id":"00000000-0000-4000-8000-000000000001","kind":"prepare-a-deposit-key","subject":{"id":"runner-deposit-key"},"steps":[{"spec_slug":"enroll","status":"completed","metadata":metadata}]});
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut reader_identity = false;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, raw)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("x-boss-user")
                {
                    let actor: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
                    reader_identity =
                        actor["role"] == "audit-readonly" && actor["access_tier"] == "auditor";
                }
            }
            assert!(
                reader_identity,
                "the enrollment read must carry the existing read-only identity"
            );
            let body = packet.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let output = Command::new("bash")
            .arg(repo_root().join("infra/gcp/runner-credential-send.sh"))
            .env(
                "BOSS_RUNNER_DEPOSIT_ENROLLMENT",
                "00000000-0000-4000-8000-000000000001",
            )
            .env("BOSS_RUNNER_DEPOSIT_KEY", &key)
            .env("BOSS_RUNNER_DEPOSIT_KNOWN_HOSTS", known)
            .env("BOSS_JOBS_URL", format!("http://{address}"))
            .env("BOSS_MACHINE_TOKEN_DIR", root.join("absent-token"))
            .output()
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            output.status.code(),
            Some(78),
            "{field}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("enrollment"));
    }
}

#[test]
fn installed_file_is_resolved_before_owner_acknowledgment_and_completion() {
    installed_delivery_case("positive");
}

#[test]
fn owner_and_cas_refusals_preserve_installed_bytes_without_claiming_delivery() {
    installed_delivery_case("owner-refusal");
    installed_delivery_case("cas-refusal");
}

#[test]
fn lost_ack_retry_observes_the_original_completed_delivery_receipt() {
    installed_delivery_case("completed-replay");
    installed_delivery_case("completed-mismatch");
}

#[test]
fn duplicate_transport_keys_are_refused_before_installation() {
    installed_delivery_case("duplicate-envelope");
}

fn installed_delivery_case(mode: &'static str) {
    use std::io::{BufRead, BufReader, Read};
    let root = scratch_dir("authenticated-runner-install");
    let destination = root.join("credential");
    write_file(&destination, "fake-existing-host-value");
    let value = "fake-sensitive-input-000000000000000000";
    let job = "00000000-0000-4000-8000-000000000001";
    let attempt = "00000000-0000-4000-8000-000000000002";
    let uid = "00000000-0000-4000-8000-000000000003";
    let step = "00000000-0000-4000-8000-000000000004";
    let credential = "ops-runner-credential-boss-gcp";
    let bound = serde_json::json!({"credential_id":credential,"job_id":job,"attempt":attempt,"secret_uid":uid});
    let input = serde_json::json!({"version":1,"host":"boss-gcp","credential_id":credential,"job_id":job,"attempt":attempt,"secret_uid":uid,"value":value});
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_server = stopped.clone();
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_requests = requests.clone();
    let installed = destination.clone();
    let server = std::thread::spawn(move || {
        let mut paths = Vec::new();
        let receipt = serde_json::json!({"version":1,"credential_id":credential,"phase":"verified","actor":"automation:ops-runner","observation_id":format!("{attempt}:delivered"),"event_id":"original-event","timestamp":"original-time","evidence_json":serde_json::json!({"purpose":"authenticated-runner-delivery","job_id":job,"host":"boss-gcp","attempt":attempt}).to_string()});
        for index in 0..if mode == "owner-refusal" || mode.starts_with("completed-") {
            5
        } else {
            6
        } {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop_server.load(std::sync::atomic::Ordering::SeqCst) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            count_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let path = request.split_whitespace().nth(1).unwrap().to_string();
            paths.push(path.clone());
            let mut length = 0;
            let mut presented = false;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, raw)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = raw.trim().parse::<usize>().unwrap();
                    }
                    if name.eq_ignore_ascii_case("x-boss-runner-credential") {
                        presented = raw.trim() == value;
                    }
                }
            }
            assert!(
                presented,
                "request {index} must authenticate the delivered value"
            );
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            if index >= 2 {
                assert_eq!(std::fs::read_to_string(&installed).unwrap(), value);
            }
            let (mut status, response) = match index {
                0 | 2 => (
                    200,
                    serde_json::json!({"resolved":true,"host":"boss-gcp","slot":"next","delivery":bound}),
                ),
                1 => (
                    200,
                    serde_json::json!({"status":"open","subject":{"id":credential},"steps":[{"id":step,"spec_slug":"delivered","status":if mode.starts_with("completed-") { "completed" } else { "ready" }}]}),
                ),
                3 => (
                    200,
                    serde_json::json!({"step":{"status":if mode.starts_with("completed-") { "completed" } else { "ready" },"assignee_id":null,"metadata":{"delivered_receipt":if mode == "completed-mismatch" { let mut wrong=receipt.clone(); wrong["event_id"]=serde_json::json!("foreign-event"); wrong } else { receipt.clone() }}},"version":"original-version"}),
                ),
                4 => {
                    let command: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(
                        command,
                        serde_json::json!({"job_id":job,"attempt":attempt,"secret_uid":uid})
                    );
                    (
                        202,
                        serde_json::json!({"recorded":true,"observation":{"outcome":"recorded","receipt":{"version":1,"credential_id":credential,"phase":"verified","actor":"automation:ops-runner","observation_id":format!("{attempt}:delivered"),"event_id":"original-event","timestamp":"original-time","evidence_json":serde_json::json!({"purpose":"authenticated-runner-delivery","job_id":job,"host":"boss-gcp","attempt":attempt}).to_string()}}}),
                    )
                }
                _ => {
                    let command: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(command["expected"]["version"], "original-version");
                    assert_eq!(
                        command["evidence"]["delivered_receipt"]["event_id"],
                        "original-event"
                    );
                    (200, serde_json::json!({"outcome":"completed"}))
                }
            };
            if (index == 4 && mode == "owner-refusal") || (index == 5 && mode == "cas-refusal") {
                status = 409;
            }
            assert!(!String::from_utf8_lossy(&bytes).contains(value));
            let body = response.to_string();
            write!(
                stream,
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        assert_eq!(paths[0], "/api/jobs/runner-credential");
        assert_eq!(paths[2], "/api/jobs/runner-credential");
        assert_eq!(paths[4], format!("/api/credentials/{credential}/delivery"));
        if mode != "owner-refusal" && !mode.starts_with("completed-") {
            assert_eq!(
                paths[5],
                format!("/api/jobs/{job}/steps/{step}/complete-if")
            );
        }
    });
    let mut child = Command::new("bash")
        .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .env("BOSS_JOBS_URL", format!("http://{address}"))
        .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-machine-token"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let wire = if mode == "duplicate-envelope" {
        format!("{{\"host\":\"forge\",{}", &input.to_string()[1..])
    } else {
        input.to_string()
    };
    feed_stdin(&mut child, wire.as_bytes());
    let output = child.wait_with_output().unwrap();
    stopped.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        output.status.code(),
        Some(if mode == "duplicate-envelope" {
            65
        } else if mode == "positive" || mode == "completed-replay" {
            0
        } else {
            1
        }),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().unwrap();
    if mode == "duplicate-envelope" {
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read_to_string(destination).unwrap(),
            "fake-existing-host-value"
        );
        return;
    }
    if mode != "positive" && mode != "completed-replay" {
        assert!(!String::from_utf8_lossy(&output.stdout).contains("delivery recorded"));
    } else {
        let returned: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            returned["original_delivery_receipt"]["event_id"],
            "original-event"
        );
        assert_eq!(
            returned["original_delivery_receipt"]["observation_id"],
            format!("{attempt}:delivered")
        );
        assert_eq!(returned["job_id"], job);
        assert_eq!(returned["attempt"], attempt);
        assert_eq!(returned["secret_uid"], uid);
    }
    assert_eq!(std::fs::read_to_string(destination).unwrap(), value);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(value));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(value));
}

#[test]
fn an_unresolved_transport_value_never_replaces_the_installed_file() {
    use std::io::{BufRead, BufReader};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        assert!(request.starts_with("GET /api/jobs/runner-credential "));
        let mut credential_seen = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if line
                .to_ascii_lowercase()
                .starts_with("x-boss-runner-credential:")
            {
                credential_seen = line.contains("fake-sensitive-input");
            }
        }
        assert!(credential_seen);
        let body = "{\"resolved\":false}";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let root = scratch_dir("unresolved-runner-deposit");
    let destination = root.join("credential");
    write_file(&destination, "fake-existing-host-value");
    let input = serde_json::json!({"version":1,"host":"boss-gcp","credential_id":"ops-runner-credential-boss-gcp","job_id":"00000000-0000-4000-8000-000000000001","attempt":"00000000-0000-4000-8000-000000000002","secret_uid":"00000000-0000-4000-8000-000000000003","value":"fake-sensitive-input-000000000000000000"});
    let mut child = Command::new("bash")
        .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .env("BOSS_JOBS_URL", format!("http://{address}"))
        .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-machine-token"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, input.to_string().as_bytes());
    let output = child.wait_with_output().unwrap();
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(65));
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "fake-existing-host-value"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fake-sensitive-input"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fake-sensitive-input"));
}

#[test]
fn resolver_host_attempt_uid_and_rotation_must_match_the_transport_exactly() {
    use std::io::{BufRead, BufReader};
    let input = serde_json::json!({"version":1,"host":"boss-gcp","credential_id":"ops-runner-credential-boss-gcp","job_id":"00000000-0000-4000-8000-000000000001","attempt":"00000000-0000-4000-8000-000000000002","secret_uid":"00000000-0000-4000-8000-000000000003","value":"fake-sensitive-input-000000000000000000"});
    for field in ["host", "attempt", "secret_uid", "job_id", "credential_id"] {
        let root = scratch_dir(&format!("runner-resolver-{field}"));
        let destination = root.join("credential");
        write_file(&destination, "fake-existing-host-value");
        let mut delivery = input.clone();
        for name in ["version", "host", "value"] {
            delivery.as_object_mut().unwrap().remove(name);
        }
        let mut response = serde_json::json!({"resolved":true,"host":"boss-gcp","slot":"next","delivery":delivery});
        if field == "host" {
            response["host"] = serde_json::json!("forge");
        } else {
            response["delivery"][field] = serde_json::json!("foreign-binding");
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            let body = response.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let mut child = Command::new("bash")
            .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
            .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
            .env("BOSS_JOBS_URL", format!("http://{address}"))
            .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-machine-token"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        feed_stdin(&mut child, input.to_string().as_bytes());
        let output = child.wait_with_output().unwrap();
        server.join().unwrap();
        assert_eq!(output.status.code(), Some(65), "{field}");
        assert_eq!(
            std::fs::read_to_string(destination).unwrap(),
            "fake-existing-host-value"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("fake-sensitive-input"));
    }
}

#[test]
fn transport_cannot_choose_another_host_destination_or_command() {
    for (name, input) in [
        (
            "foreign-host",
            serde_json::json!({"version":1,"host":"forge","credential_id":"ops-runner-credential-boss-gcp","job_id":"00000000-0000-4000-8000-000000000001","attempt":"00000000-0000-4000-8000-000000000002","secret_uid":"00000000-0000-4000-8000-000000000003","value":"fake-sensitive-input-000000000000000000"}),
        ),
        (
            "extra-command",
            serde_json::json!({"version":1,"host":"boss-gcp","credential_id":"ops-runner-credential-boss-gcp","job_id":"00000000-0000-4000-8000-000000000001","attempt":"00000000-0000-4000-8000-000000000002","secret_uid":"00000000-0000-4000-8000-000000000003","value":"fake-sensitive-input-000000000000000000","command":"touch forbidden"}),
        ),
    ] {
        let root = scratch_dir(name);
        let destination = root.join("credential");
        write_file(&destination, "fake-existing-host-value");
        let mut child = Command::new("bash")
            .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
            .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
            .env("BOSS_JOBS_URL", "http://127.0.0.1:1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        feed_stdin(&mut child, input.to_string().as_bytes());
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(65));
        assert_eq!(
            std::fs::read_to_string(destination).unwrap(),
            "fake-existing-host-value"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("fake-sensitive-input"));
    }
}

#[test]
fn an_empty_runner_deposit_preserves_the_installed_credential() {
    let root = scratch_dir("empty-runner-deposit");
    let destination = root.join("ops-runner.credential");
    write_file(&destination, "fake-existing-host-value");
    let mut child = Command::new("bash")
        .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, b"");
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(65),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "fake-existing-host-value"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fake-existing-host-value"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fake-existing-host-value"));
}

#[test]
fn an_oversized_runner_deposit_refuses_without_echoing_or_replacing_it() {
    let root = scratch_dir("oversized-runner-deposit");
    let destination = root.join("ops-runner.credential");
    write_file(&destination, "fake-existing-host-value");
    let input = "fake-sensitive-input".repeat(1000);
    let mut child = Command::new("bash")
        .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // An early bounded refusal may close stdin before the writer finishes.
    feed_stdin(&mut child, input.as_bytes());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(65));
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "fake-existing-host-value"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fake-sensitive-input"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fake-sensitive-input"));
}
