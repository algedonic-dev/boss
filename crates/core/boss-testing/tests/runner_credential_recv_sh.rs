//! The separate boss-gcp runner receiver refuses malformed transport before
//! replacing the host's working credential. No recovery receiver is reused.
use boss_testing::{feed_stdin, repo_root, scratch_dir, without_proxy, write_exec, write_file};
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

/// The runner deposit key's forced command, read from its rule row —
/// the one definition the broker joins into the enroll step's line.
fn runner_forced_command() -> String {
    let rule: toml::Value = toml::from_str(
        &std::fs::read_to_string(
            repo_root().join("infra/dispatcher/rules/broker-prepares-the-runner-deposit-key.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    rule["rule"][0]["do"][0]["args"]["forced_command"]
        .as_str()
        .expect("the runner rule declares its forced command")
        .trim_matches('"')
        .to_string()
}

/// The receiver's first line, which it says before it reads a byte
/// (backlog dea2236f), read from the receiver itself.
fn receiver_greeting() -> String {
    let body =
        std::fs::read_to_string(repo_root().join("infra/gcp/runner-credential-recv.sh")).unwrap();
    body.lines()
        .find_map(|l| l.strip_prefix("GREETING='"))
        .and_then(|rest| rest.strip_suffix('\''))
        .expect("the receiver declares its GREETING")
        .to_string()
}

/// The receiver's stdout: its greeting, then the one receipt it answers.
fn receipt_after_greeting(stdout: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stdout);
    let (first, rest) = text
        .split_once('\n')
        .unwrap_or_else(|| panic!("the receiver said one line or none: {text}"));
    assert_eq!(first, receiver_greeting(), "the receiver speaks first");
    serde_json::from_str(rest)
        .unwrap_or_else(|e| panic!("no receipt after the greeting: {e}: {rest}"))
}

/// The line the broker writes onto the enroll step for `public`.
fn runner_line(public: &str) -> String {
    format!("command=\"{}\",restrict {public}", runner_forced_command())
}

/// A prepare-a-deposit-key v2 packet as the jobs API serves it to the
/// sender: the completed install receipt naming `public`, and `enroll`
/// carrying `enroll` (item 3dc0b247: the line, never a bare key).
fn enrollment_packet(public: &str, enroll: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"id":"00000000-0000-4000-8000-000000000001","kind":"prepare-a-deposit-key",
        "subject":{"id":"runner-deposit-key"},"steps":[
        {"spec_slug":"install","status":"completed","metadata":{"installed":"read back","public_key":public}},
        {"spec_slug":"enroll","status":"completed","metadata":enroll}]})
}

/// The sender's receiver command is the rule row's forced command: two
/// copies of one fact (the rule the broker reads, the constant the
/// sender checks the signed line with), pinned equal here.
#[test]
fn the_senders_receiver_command_is_the_rules_forced_command() {
    let sender =
        std::fs::read_to_string(repo_root().join("infra/gcp/runner-credential-send.sh")).unwrap();
    let forced = runner_forced_command();
    assert!(
        sender.contains(&format!("RECEIVER_COMMAND='{forced}'\n")),
        "runner-credential-send.sh checks the signed line against a different command than the rule's {forced:?}"
    );
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
        // The receiver as this tree installs it: it speaks first, then
        // reads the envelope, then answers with its receipt.
        write_exec(
            &bin.join("ssh"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$FIXTURE_ROOT/ssh-args\"\nprintf '%s\\n' '{}'\ncat > \"$FIXTURE_ROOT/wire\"\ncat \"$FIXTURE_ROOT/receipt\"\n",
                receiver_greeting()
            ),
        );
        let mut packet = enrollment_packet(
            &public,
            serde_json::json!({"authorized_keys_line":runner_line(&public),"receiver_host":"boss-gcp","purpose":"ops-runner credential deposit","decision":"approved","enrollment_record":"exact-purpose-install-record"}),
        );
        match enrollment_id {
            Some(id) => packet["id"] = serde_json::json!(id),
            None => {
                packet.as_object_mut().unwrap().remove("id");
            }
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
        let output = without_proxy(&mut Command::new("bash"))
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
    // The stage/promote pair is live now, in infra/dispatcher/rules, and
    // pinned there by runner_credential_send_sh.rs (backlog 1e50e66b).
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
    let output = without_proxy(&mut Command::new("bash"))
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
    // Each enroll field, the install receipt's key, and the two shapes
    // v2 retired (item 3dc0b247): a bare key in place of the line, and a
    // line that forces another command.
    for field in [
        "receiver_host",
        "purpose",
        "authorized_keys_line",
        "decision",
        "enrollment_record",
        "install.public_key",
        "bare key, no line",
        "another forced command",
    ] {
        let slug: String = field
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let root = scratch_dir(&format!("sender-enrollment-{slug}"));
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
        let mut metadata = serde_json::json!({"authorized_keys_line":runner_line(&public),"receiver_host":"boss-gcp","purpose":"ops-runner credential deposit","decision":"approved","enrollment_record":"recorded-exact-receiver"});
        let mut installed = public.clone();
        match field {
            "enrollment_record" => metadata[field] = serde_json::json!(""),
            "install.public_key" => installed = "ssh-ed25519 AAAAmismatch".into(),
            "bare key, no line" => {
                metadata
                    .as_object_mut()
                    .unwrap()
                    .remove("authorized_keys_line");
                metadata["public_key"] = serde_json::json!(public);
            }
            "another forced command" => {
                metadata["authorized_keys_line"] = serde_json::json!(format!(
                    "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict {public}"
                ))
            }
            _ => metadata[field] = serde_json::json!("mismatch"),
        }
        let packet = enrollment_packet(&installed, metadata);
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
        let output = without_proxy(&mut Command::new("bash"))
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

/// A REFUSED PACKET READ IS REFUSED BY NAME (backlog b675ad09). The jobs
/// API answering the rotation packet read 403 is what refused both
/// boss-gcp deliveries on 2026-10-10 (forge converges ec127c89 and
/// f02e5a33), and the record carried the one bare sentence every check
/// shared. The refusal names the check and its status; nothing is
/// installed.
#[test]
fn a_refused_rotation_packet_read_is_refused_naming_its_status() {
    installed_delivery_case("packet-refused");
}

/// INSTALLED FLIPS AT THE RENAME, NOT BEFORE IT (backlog a61cce40, a low
/// of review run e5d18320 on car 7b2daccc). The workdir case above pins
/// the earliest failure, the directory-sync case below the first one
/// after the rename, and the delivery cases the acknowledgment's; nothing
/// pinned the window between, and with the flip moved up
/// to just before the staging mktemp every case here still passed, while
/// staging-file, staging-write, staging-mode, staging-owner, staging-sync
/// and install-rename would each have told the operator the delivered
/// credential IS installed with the old one still in place. Planted
/// without editing the receiver: the destination is a DIRECTORY, so the
/// door resolves, the packet read answers, the staging file is written
/// and synced, and `mv -fT` refuses to put a file over it — the last act
/// before the flip, so a flip placed anywhere ahead of the rename reads
/// wrong here. The staging file is cleaned up, not left beside it.
#[test]
fn a_failed_rename_says_the_installed_credential_is_unchanged() {
    installed_delivery_case("rename-refused");
}

/// ...AND NOT AFTER THE DIRECTORY SYNC THAT FOLLOWS IT (backlog 533bc37c,
/// from review run eb565e75 of car 06935c9d). The comment above once said
/// the delivery cases pinned every failure after the rename; they pin
/// only the acknowledgment's, which come after `sync -- "$DIR"`. With the
/// flip moved one line late, below that sync, all 27 cases stayed green
/// while an install-directory-sync failure would tell the operator the
/// installed credential was unchanged — with the delivered one already
/// in place. Planted without editing the receiver: a `sync` on PATH that
/// refuses a directory and passes a file, so the staging sync and the
/// rename succeed and the directory sync is the one that fails.
#[test]
fn a_failed_directory_sync_says_the_delivered_credential_is_installed() {
    installed_delivery_case("directory-sync-failed");
}

/// EVERY REFUSAL AND EVERY FAILURE NAMES ITS CHECK, AND NO TWO SHARE A
/// WORD (backlog b675ad09; backlog e317cbe2, from review 9bbf4e03 of car
/// b970a8fd). Read off the receiver itself: no `refuse` and no `fail`
/// call is bare, so a check added later cannot fall back to a sentence
/// that says only THAT it refused or failed. The scan covered `refuse`
/// alone until e317cbe2, and eleven bare `fail`s survived it — among
/// them the staging, rename and directory sync that can leave a
/// credential installed and unacknowledged with no cause on the record.
/// And a word two calls share names neither: `not-one-document`,
/// `unread` and `envelope-unread` each stood for two or three checks.
#[test]
fn every_refusal_and_every_failure_in_the_receiver_names_its_own_check() {
    let body =
        std::fs::read_to_string(repo_root().join("infra/gcp/runner-credential-recv.sh")).unwrap();
    let mut words: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (verb, floor) in [("refuse", 20), ("fail", 12)] {
        // `|| fail; fi` is a call too: the word ends at a separator as
        // well as at a space, and the first draft of this scan missed
        // line 171's chown for want of it.
        let call = regex::Regex::new(&format!(r"(^|[|&;{{(]\s*){verb}(?P<rest>([\s;}}|&)].*)?)$"))
            .unwrap();
        let mut calls = 0;
        let mut bare = Vec::new();
        for (n, line) in body.lines().enumerate() {
            let live = line.trim_start();
            if live.starts_with('#') || live.starts_with(&format!("{verb}()")) {
                continue;
            }
            for c in call.captures_iter(line) {
                calls += 1;
                let rest = c["rest"].trim_start();
                if rest.is_empty() || rest.starts_with([';', '}', '|', '&', ')']) {
                    bare.push(format!("line {}: {line}", n + 1));
                    continue;
                }
                let word = rest
                    .split(|ch: char| ch.is_whitespace() || ch == ';' || ch == '}')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                words.entry(word).or_default().push(n + 1);
            }
        }
        assert!(
            calls >= floor,
            "the scan found {calls} {verb} calls — the fixture is not reading the receiver"
        );
        assert!(
            bare.is_empty(),
            "a {verb} that does not name its check:\n{}",
            bare.join("\n")
        );
    }
    let shared: Vec<String> = words
        .iter()
        .filter(|(_, lines)| lines.len() > 1)
        .map(|(word, lines)| format!("{word} on lines {lines:?}"))
        .collect();
    assert!(
        shared.is_empty(),
        "a reason word more than one check answers with names neither:\n{}",
        shared.join("\n")
    );
}

/// A FAILURE SAYS WHETHER THE CREDENTIAL WAS REPLACED (backlog e317cbe2).
/// Before the rename nothing is installed, and the line says the
/// installed credential is unchanged; after it, every failure line says
/// the delivered credential IS installed and unacknowledged, which the
/// delivery cases below read. Planted here: a temp directory the
/// receiver cannot make, its first act.
#[test]
fn a_failure_before_installation_says_the_installed_credential_is_unchanged() {
    let root = scratch_dir("runner-recv-no-workdir");
    let destination = root.join("credential");
    write_file(&destination, "fake-existing-host-value");
    let output = without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
        .env("TMPDIR", root.join("absent"))
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .env("BOSS_JOBS_URL", "http://127.0.0.1:1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "runner-credential-recv: failed (workdir); installed credential unchanged; delivery not acknowledged"
        ),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "fake-existing-host-value"
    );
}

/// The id the receiver signs as, and the row that answers for it
/// (backlog e317cbe2): its own, not the forge deposit's.
const RECV_ACTOR: &str = "automation:runner-credential-recv";

/// THE RECEIVER SIGNS UNDER A ROW THAT DESCRIBES IT (backlog e317cbe2,
/// from review 9bbf4e03 of car b970a8fd). It signed as
/// automation:runner-credential-deposit, whose row describes the FORGE's
/// deposit script, so a reader of that row learned nothing about
/// boss-gcp. The delivery fixture reads the id off every request.
#[test]
fn the_receiver_signs_as_its_own_registered_automation() {
    let rows = boss_jobs::agents::automations::load_automations_dir(
        &repo_root().join("infra/platform/automations"),
    )
    .unwrap();
    let row = rows
        .iter()
        .find(|row| row.id == RECV_ACTOR)
        .unwrap_or_else(|| panic!("no automation row answers for {RECV_ACTOR}"));
    assert!(
        row.description
            .contains("infra/gcp/runner-credential-recv.sh"),
        "the row names the script that signs as it: {}",
        row.description
    );
    assert_eq!(row.role, "platform-admin");
}

/// THE DELIVERY OWNER REFUSES ANY TIER BUT OPERATOR, AND SO DOES THE
/// FIXTURE (backlog e317cbe2, from review 9bbf4e03 of car b970a8fd).
/// `record_delivery` (boss-jobs credentials/http.rs) answers 403 unless
/// the request asserts `access_tier: operator`; the fixture answered 202
/// to anything, so a receiver with the tier removed passed every case
/// here and would fail every live delivery. Planted in an installed copy
/// with the tier cut out of its x-boss-user: the owner refuses, nothing
/// is acknowledged, and the line says so with the status.
#[test]
fn a_delivery_asserting_no_operator_tier_is_refused_as_the_owner_refuses_it() {
    let dir = scratch_dir("runner-recv-no-operator-tier");
    let out = install_runner_receiver(&dir, true, "david");
    assert!(out.status.success(), "{}", both(&out));
    let receiver = installed_receiver(&dir.join("libexec"));
    let text = std::fs::read_to_string(&receiver).unwrap();
    let tier = r#""access_tier":"operator","#;
    assert!(text.contains(tier), "the plant needs the tier");
    write_file(&receiver, &text.replace(tier, ""));
    installed_delivery_case_at(&receiver, "tier-dropped");
}

/// The reason a refusal names, read off its one stderr line.
fn refused_naming(stderr: &[u8], reason: &str) -> bool {
    String::from_utf8_lossy(stderr).contains(&format!(
        "runner-credential-recv: refused ({reason}); installed credential unchanged"
    ))
}

fn installed_delivery_case(mode: &'static str) {
    installed_delivery_case_at(
        &repo_root().join("infra/gcp/runner-credential-recv.sh"),
        mode,
    );
}

/// The delivery case, driving the receiver at `receiver`: the tree's
/// file, or the root-owned copy the boss-gcp installer places, which
/// must resolve every file it sources from beside itself.
fn installed_delivery_case_at(receiver: &std::path::Path, mode: &'static str) {
    use std::io::{BufRead, BufReader, Read};
    let root = scratch_dir("authenticated-runner-install");
    let destination = root.join("credential");
    if mode == "rename-refused" {
        // A directory at the destination: `mv -fT` cannot replace it.
        std::fs::create_dir(&destination).unwrap();
        write_file(&destination.join("held"), "fake-existing-host-value");
    } else {
        write_file(&destination, "fake-existing-host-value");
    }
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
        for index in 0..if mode == "packet-refused"
            || mode == "rename-refused"
            || mode == "directory-sync-failed"
        {
            2
        } else if mode == "owner-refusal"
            || mode == "tier-dropped"
            || mode.starts_with("completed-")
        {
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
            let mut asserted_role = None;
            let mut asserted_tier = None;
            let mut asserted_id = None;
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
                    if name.eq_ignore_ascii_case("x-boss-user") {
                        let actor: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
                        asserted_role = actor["role"].as_str().map(str::to_string);
                        asserted_tier = actor["access_tier"].as_str().map(str::to_string);
                        asserted_id = actor["id"].as_str().map(str::to_string);
                    }
                }
            }
            assert!(
                presented,
                "request {index} must authenticate the delivered value"
            );
            assert_eq!(
                asserted_id.as_deref(),
                Some(RECV_ACTOR),
                "request {index} signs as the receiver's own row (backlog e317cbe2)"
            );
            // The credential door keeps the role and tier a request
            // asserts and only binds its actor (boss-jobs
            // runner_credential.rs), so the fixture refuses on them as the
            // live jobs API does, on EVERY request rather than the packet
            // read alone (backlog e317cbe2). A request asserting no role is
            // the guest, which holds no job read or write: the live API
            // answered the rotation packet read 403 for exactly that on
            // both boss-gcp deliveries (backlog b675ad09). And the delivery
            // owner, `record_delivery` in credentials/http.rs, refuses any
            // tier but operator.
            let refused_by_policy = match index {
                1 | 3 | 5 => asserted_role.as_deref() != Some("platform-admin"),
                4 => asserted_tier.as_deref() != Some("operator"),
                _ => false,
            };
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
            if (index == 1 && mode == "packet-refused") || refused_by_policy {
                status = 403;
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
        if mode == "packet-refused" || mode == "rename-refused" || mode == "directory-sync-failed" {
            assert_eq!(paths[1], format!("/api/jobs/{job}"));
            return;
        }
        assert_eq!(paths[2], "/api/jobs/runner-credential");
        assert_eq!(paths[4], format!("/api/credentials/{credential}/delivery"));
        if mode != "owner-refusal" && mode != "tier-dropped" && !mode.starts_with("completed-") {
            assert_eq!(
                paths[5],
                format!("/api/jobs/{job}/steps/{step}/complete-if")
            );
        }
    });
    let mut command = Command::new("bash");
    without_proxy(&mut command)
        .arg(receiver)
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .env("BOSS_JOBS_URL", format!("http://{address}"))
        .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-machine-token"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if mode == "directory-sync-failed" {
        // A `sync` that refuses a DIRECTORY and passes a file: the
        // staging sync of the file succeeds and the rename runs, then the
        // sync of the destination's directory is the one that fails. It
        // records every argument it was handed, so the case below can
        // say the file sync ran too, not only that one call failed.
        let bin = scratch_dir("runner-recv-directory-sync-stub");
        write_exec(
            &bin.join("sync"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nfor a; do last=$a; done\n[ ! -d \"$last\" ]\n",
                root.join("sync-calls").display()
            ),
        );
        command.env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
    }
    let mut child = command.spawn().unwrap();
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
        Some(
            if mode == "duplicate-envelope" || mode == "packet-refused" {
                65
            } else if mode == "positive" || mode == "completed-replay" {
                0
            } else {
                1
            }
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().unwrap();
    if mode == "duplicate-envelope" {
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            refused_naming(&output.stderr, "duplicate-key"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(destination).unwrap(),
            "fake-existing-host-value"
        );
        return;
    }
    if mode == "packet-refused" {
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(
            refused_naming(&output.stderr, "rotation-packet-http-403"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(destination).unwrap(),
            "fake-existing-host-value"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains(value));
        return;
    }
    if mode == "rename-refused" {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(
            stderr.contains(
                "runner-credential-recv: failed (install-rename); installed credential unchanged; delivery not acknowledged"
            ),
            "{stderr}"
        );
        assert!(!stderr.contains("IS installed"), "{stderr}");
        assert!(!stderr.contains(value));
        assert_eq!(
            std::fs::read_to_string(destination.join("held")).unwrap(),
            "fake-existing-host-value"
        );
        let staged: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".runner-recv."))
            .collect();
        assert!(staged.is_empty(), "staging left behind: {staged:?}");
        return;
    }
    if mode == "directory-sync-failed" {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        let calls = std::fs::read_to_string(root.join("sync-calls")).unwrap();
        assert_eq!(
            calls.lines().count(),
            2,
            "the staging file's sync and the directory's, nothing else: {calls}"
        );
        assert!(
            stderr.contains(
                "runner-credential-recv: failed (install-directory-sync); the delivered credential IS installed; delivery not acknowledged"
            ),
            "{stderr}"
        );
        assert!(
            !stderr.contains("installed credential unchanged"),
            "{stderr}"
        );
        assert!(!stderr.contains(value));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(value));
        assert_eq!(std::fs::read_to_string(destination).unwrap(), value);
        return;
    }
    if mode != "positive" && mode != "completed-replay" {
        assert!(!String::from_utf8_lossy(&output.stdout).contains("delivery recorded"));
        // The acknowledgment's own sentence rides the failure, so the
        // record says which owner step refused (backlog b675ad09).
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("runner-credential-recv: failed (not recorded: "),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // After the rename the line says the credential WAS replaced, so
        // an installed-but-unacknowledged host is on the record as one
        // (backlog e317cbe2).
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("; the delivered credential IS installed; delivery not acknowledged"),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if mode == "tier-dropped" {
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("authenticated delivery owner refused or its original receipt is unavailable (HTTP 403)"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    } else {
        let returned = receipt_after_greeting(&output.stdout);
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
    let mut child = without_proxy(&mut Command::new("bash"))
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
    assert!(
        refused_naming(&output.stderr, "door-not-this-delivery"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "fake-existing-host-value"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fake-sensitive-input"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fake-sensitive-input"));
}

/// THE TRANSPORT'S VALUE IS PRESENTED ONLY TO A HOST THE MACHINE TOKEN'S
/// RULE ADMITS (backlog 50708d76, F2 of review 927f8602). The receiver
/// made the credential's header with plain `secret_header`, which judges
/// no host, at both of its presentations; it asks `machine_token_admits`
/// first now, and a host outside the rule is a refusal with the installed
/// file unchanged.
///
/// Read by EFFECT with the real curl: a loopback listener that either
/// spelling reaches — `127.1`, which curl expands and the rule withholds,
/// and a bare `127.0.0.1:<port>`, which has no leading scheme (F1) — must
/// see NO connection. The control is the same listener and the same input
/// spelled `http://127.0.0.1:<port>`: the connection arrives carrying the
/// header, so the silence above is the judgement and not the fixture.
#[test]
fn a_transport_value_is_never_presented_to_a_host_the_machine_token_rule_does_not_admit() {
    use std::io::{BufRead, BufReader};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = scratch_dir("off-estate-runner-deposit");
    let destination = root.join("credential");
    write_file(&destination, "fake-existing-host-value");
    let input = serde_json::json!({"version":1,"host":"boss-gcp","credential_id":"ops-runner-credential-boss-gcp","job_id":"00000000-0000-4000-8000-000000000001","attempt":"00000000-0000-4000-8000-000000000002","secret_uid":"00000000-0000-4000-8000-000000000003","value":"fake-sensitive-input-000000000000000000"});
    let receive = |jobs_url: String| {
        let mut child = without_proxy(&mut Command::new("bash"))
            .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
            .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
            .env("BOSS_JOBS_URL", jobs_url)
            .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-machine-token"))
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .env("BOSS_SOR_ENV", root.join("no-sor.env"))
            .env("BOSS_API_RETRY_DEADLINE", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        feed_stdin(&mut child, input.to_string().as_bytes());
        child.wait_with_output().unwrap()
    };

    // A connection that does arrive is counted and closed at once, so a
    // receiver that presents fails this test rather than hanging it.
    listener.set_nonblocking(true).unwrap();
    let arrived = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let (arrived, done) = (arrived.clone(), done.clone());
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering::SeqCst;
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        arrived.fetch_add(1, SeqCst);
                        drop(stream);
                    }
                    Err(_) if done.load(SeqCst) => return listener,
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        })
    };
    let mut withheld = Vec::new();
    for spelled in [format!("http://127.1:{port}"), format!("127.0.0.1:{port}")] {
        withheld.push((spelled.clone(), receive(spelled)));
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let listener = watcher.join().unwrap();
    for (spelled, output) in withheld {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            arrived.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "[{spelled}] the receiver connected, to present a credential to a host the machine \
             token's rule does not admit:\n{stderr}"
        );
        assert_eq!(output.status.code(), Some(65), "[{spelled}] {stderr}");
        assert!(
            stderr.contains("NOT presented") && stderr.contains("BOSS_MACHINE_TOKEN_HOSTS"),
            "[{spelled}] the refusal names the rule: {stderr}"
        );
        assert!(
            refused_naming(&output.stderr, "not-presented"),
            "[{spelled}] {stderr}"
        );
        assert!(!stderr.contains("fake-sensitive-input"), "{stderr}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("fake-sensitive-input"));
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "fake-existing-host-value",
            "[{spelled}] the installed credential is unchanged"
        );
    }

    // The control: the same listener, the same input, a host the rule
    // admits — the presentation arrives.
    listener.set_nonblocking(false).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut presented = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if line
                .to_ascii_lowercase()
                .starts_with("x-boss-runner-credential:")
            {
                presented = line.contains("fake-sensitive-input");
            }
        }
        let body = "{\"resolved\":false}";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        presented
    });
    let output = receive(format!("http://127.0.0.1:{port}"));
    assert!(
        server.join().unwrap(),
        "the control: an admitted host is presented the credential"
    );
    assert_eq!(output.status.code(), Some(65));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("NOT presented"));
}

/// NOR THROUGH A PROXY (backlog 50708d76, F4 of review 927f8602). An
/// admitted host is still refused when curl would send the request
/// through a proxy, which would read the header: the rule answers 2, the
/// line names the variable and not its value, the installed file is
/// unchanged, and the proxy — a loopback listener — sees no connection.
#[test]
fn a_transport_value_is_never_presented_through_a_proxy() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    proxy.set_nonblocking(true).unwrap();
    // A connection that does arrive is counted and closed at once, so a
    // receiver that presents fails this test rather than hanging it on a
    // proxy that never answers.
    let arrived = std::sync::Arc::new(AtomicUsize::new(0));
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let watcher = {
        let (arrived, done) = (arrived.clone(), done.clone());
        std::thread::spawn(move || {
            loop {
                match proxy.accept() {
                    Ok((stream, _)) => {
                        arrived.fetch_add(1, SeqCst);
                        drop(stream);
                    }
                    Err(_) if done.load(SeqCst) => return,
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        })
    };
    let root = scratch_dir("proxied-runner-recv");
    let destination = root.join("credential");
    write_file(&destination, "fake-existing-host-value");
    let input = serde_json::json!({"version":1,"host":"boss-gcp","credential_id":"ops-runner-credential-boss-gcp","job_id":"00000000-0000-4000-8000-000000000001","attempt":"00000000-0000-4000-8000-000000000002","secret_uid":"00000000-0000-4000-8000-000000000003","value":"fake-sensitive-input-000000000000000000"});
    let mut child = without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join("infra/gcp/runner-credential-recv.sh"))
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .env("BOSS_JOBS_URL", "http://127.0.0.1:1")
        .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-machine-token"))
        .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
        .env("BOSS_SOR_ENV", root.join("no-sor.env"))
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .env(
            "http_proxy",
            // Formatted in, so the no-secrets lint reads the planted login
            // as the fixture it is, not a literal password.
            format!(
                "http://proxyuser:{}@127.0.0.1:{proxy_port}",
                "proxy-pass-9e2"
            ),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, input.to_string().as_bytes());
    let output = child.wait_with_output().unwrap();
    done.store(true, SeqCst);
    watcher.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        arrived.load(SeqCst),
        0,
        "the receiver connected to the proxy: {stderr}"
    );
    assert_eq!(output.status.code(), Some(65), "{stderr}");
    assert!(
        stderr.contains("NOT presented") && stderr.contains("proxy in $http_proxy"),
        "the refusal names the proxy variable: {stderr}"
    );
    assert!(refused_naming(&output.stderr, "not-presented"), "{stderr}");
    for never in ["proxyuser", "proxy-pass-9e2", "fake-sensitive-input"] {
        assert!(!stderr.contains(never), "never {never}: {stderr}");
    }
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "fake-existing-host-value",
        "the installed credential is unchanged"
    );
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
        let mut child = without_proxy(&mut Command::new("bash"))
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
        assert!(
            refused_naming(&output.stderr, "door-not-this-delivery"),
            "{field}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
        let mut child = without_proxy(&mut Command::new("bash"))
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
        assert!(
            refused_naming(&output.stderr, "envelope-shape"),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
    let mut child = without_proxy(&mut Command::new("bash"))
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
    // Its greeting, and nothing after it: the empty envelope is refused in
    // words of its own, the evidence a verify pass reads.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{}\n", receiver_greeting())
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("refused an empty envelope"),
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
    let mut child = without_proxy(&mut Command::new("bash"))
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
    assert!(
        refused_naming(&output.stderr, "oversize"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "fake-existing-host-value"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fake-sensitive-input"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fake-sensitive-input"));
}

// ---------------------------------------------------------------------------
// The door on boss-gcp — R-b of 1e50e66b's 2026-10-09 ledger. Until this,
// nothing installed the receiver the runner deposit key's forced command
// names, and the enroll procedure sent David to a converge summary that
// no converge wrote (review c59facbf, N1).
// ---------------------------------------------------------------------------

const INSTALLER: &str = "infra/gcp/install-runner-credential-receiver.sh";

/// The words the enroll procedure sends its signer to, and the converge
/// records: one key and one value, held equal on both sides here so
/// neither can be respelled alone (CLAUDE.md §9a; review c59facbf N1).
const SUMMARY_KEY: &str = "runner_credential_receiver";
const SUMMARY_INSTALLED: &str = "installed";

fn file_mode(p: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .unwrap_or_else(|e| panic!("stat {}: {e}", p.display()))
        .permissions()
        .mode()
        & 0o777
}

/// The receiver's absolute path on boss-gcp: the rule row's forced
/// command, without its `exec sudo -n `.
fn receiver_on_the_host() -> String {
    runner_forced_command()
        .strip_prefix("exec sudo -n ")
        .expect("the runner's forced command runs the receiver through sudo -n")
        .to_string()
}

/// Where the installer's copy of that receiver lands under a scratch
/// `libexec` that stands in for the host's /usr/local/libexec/boss.
fn installed_receiver(libexec: &std::path::Path) -> std::path::PathBuf {
    libexec.join(
        receiver_on_the_host()
            .strip_prefix("/usr/local/libexec/boss/")
            .expect("the receiver lives under the host's libexec root"),
    )
}

fn install_runner_receiver(
    dir: &std::path::Path,
    visudo_ok: bool,
    user: &str,
) -> std::process::Output {
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    boss_testing::create_dir(&dir.join("sudoers.d"));
    write_exec(
        &bin.join("visudo"),
        &format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >>{d}/visudo-calls\ncp \"$2\" {d}/checked\nexit {rc}\n",
            d = dir.display(),
            rc = if visudo_ok { 0 } else { 1 }
        ),
    );
    without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join(INSTALLER))
        .env("INSTALL_RECV_LIBEXEC", dir.join("libexec"))
        .env("INSTALL_RECV_SUDOERS_DIR", dir.join("sudoers.d"))
        .env("INSTALL_VISUDO", bin.join("visudo"))
        .env("INSTALL_RECV_USER", user)
        .env("INSTALL_RECV_OWNER", "")
        .output()
        .expect("run the installer")
}

fn every_file_below(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_eq!(file_mode(&path), 0o755, "{}", path.display());
            found.extend(every_file_below(&path));
        } else {
            found.push(path);
        }
    }
    found
}

fn both(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn the_installer_places_the_receivers_copy_and_one_rule_that_takes_no_arguments() {
    let dir = scratch_dir("runner-recv-install");
    let out = install_runner_receiver(&dir, true, "david");
    assert!(out.status.success(), "{}", both(&out));

    // A COPY of the tree's files, byte for byte, each keeping the tree's
    // own execute bit, under a 0755 hierarchy — outside the checkout the
    // grantee owns.
    let copy = dir.join("libexec/runner-delivery");
    let files = every_file_below(&copy);
    assert!(
        files.contains(&installed_receiver(&dir.join("libexec"))),
        "the receiver the forced command names was not placed: {files:?}"
    );
    for file in &files {
        let rel = file.strip_prefix(&copy).unwrap();
        let source = repo_root().join(rel);
        assert_eq!(
            std::fs::read(file).unwrap(),
            std::fs::read(&source)
                .unwrap_or_else(|e| panic!("{} has no tree source: {e}", rel.display())),
            "{} is not the tree's",
            rel.display()
        );
        let executable = file_mode(&source) & 0o100 != 0;
        assert_eq!(
            file_mode(file),
            if executable { 0o755 } else { 0o644 },
            "{}",
            rel.display()
        );
    }

    // ONE rule, naming the receiver with "" — sudo's spelling of NO
    // arguments, so the grant is exactly the forced command's own line.
    let rule = dir.join("sudoers.d/boss-runner-credential-recv");
    let body = std::fs::read_to_string(&rule).unwrap();
    let lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
    let r = receiver_on_the_host();
    assert_eq!(
        lines,
        vec![
            format!("Defaults!{r} env_reset").as_str(),
            format!(
                "Defaults!{r} secure_path=\"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\""
            )
            .as_str(),
            format!("david ALL=(root) NOPASSWD: {r} \"\"").as_str(),
        ],
        "the sudoers rule grants something other than the receiver with no arguments:\n{body}"
    );
    assert_eq!(file_mode(&rule), 0o440);
    assert!(
        std::fs::read_to_string(dir.join("visudo-calls"))
            .unwrap()
            .starts_with("-cf "),
        "not checked with visudo -cf"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("checked")).unwrap(),
        body,
        "visudo checked something other than what was placed"
    );

    // A second pass replaces the copy whole and leaves no staging behind.
    let out = install_runner_receiver(&dir, true, "david");
    assert!(out.status.success(), "{}", both(&out));
    let left: Vec<String> = std::fs::read_dir(dir.join("libexec"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(left, vec!["runner-delivery".to_string()], "{left:?}");
    assert_eq!(every_file_below(&copy).len(), files.len());
}

/// The copy runs on its own: every file the receiver sources, and every
/// file those source, was placed beside it. Driven through the same
/// delivery the tree's receiver passes above, from the INSTALLED path.
#[test]
fn the_installed_copy_delivers_from_beside_itself() {
    let dir = scratch_dir("runner-recv-installed-copy");
    let out = install_runner_receiver(&dir, true, "david");
    assert!(out.status.success(), "{}", both(&out));
    installed_delivery_case_at(&installed_receiver(&dir.join("libexec")), "positive");
}

/// A LIBRARY THAT LACKS A FUNCTION THE RECEIVER CALLS IS NAMED, NEVER
/// BLAMED ON THE HOSTS LIST (backlog 50708d76, a low of review run
/// ce15f459 on car 578fc323). The receiver runs a copy whose libraries sit beside it, and a
/// secret-header.sh from before `machine_token_admits` existed answered
/// that call with bash's 127, which the presentation read as "not
/// admitted": every delivery refused, exit 65, naming
/// BOSS_MACHINE_TOKEN_HOSTS — a list that was right all along. The
/// installer now swaps the whole set from one checkout, so the shape is a
/// copy edited or placed by hand; planted here by renaming the definition
/// in an installed copy. The receiver must name the function and the
/// file, fail (exit 1, nothing acknowledged) rather than refuse its input,
/// and leave the installed credential as it was.
#[test]
fn a_library_missing_a_function_the_receiver_calls_is_named_and_not_blamed_on_the_host_list() {
    let dir = scratch_dir("runner-recv-mixed-set");
    let out = install_runner_receiver(&dir, true, "david");
    assert!(out.status.success(), "{}", both(&out));
    let lib = dir.join("libexec/runner-delivery/infra/lib/secret-header.sh");
    let text = std::fs::read_to_string(&lib).unwrap();
    let definition = "\nmachine_token_admits() {";
    assert!(text.contains(definition), "the plant needs the definition");
    write_file(
        &lib,
        &text.replace(definition, "\nnot_this_trees_machine_token_admits() {"),
    );
    let destination = dir.join("credential");
    write_file(&destination, "fake-existing-host-value");
    let input = serde_json::json!({"version":1,"host":"boss-gcp","credential_id":"ops-runner-credential-boss-gcp","job_id":"00000000-0000-4000-8000-000000000001","attempt":"00000000-0000-4000-8000-000000000002","secret_uid":"00000000-0000-4000-8000-000000000003","value":"fake-sensitive-input-000000000000000000"});
    let mut child = without_proxy(&mut Command::new("bash"))
        .arg(installed_receiver(&dir.join("libexec")))
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .env("BOSS_JOBS_URL", "http://127.0.0.1:1")
        .env("BOSS_MACHINE_TOKEN_DIR", dir.join("no-machine-token"))
        .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
        .env("BOSS_SOR_ENV", dir.join("no-sor.env"))
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, input.to_string().as_bytes());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("lib/secret-header.sh defines no machine_token_admits"),
        "the failure names the function and the file: {stderr}"
    );
    assert!(
        !stderr.contains("BOSS_MACHINE_TOKEN_HOSTS") && !stderr.contains("NOT presented"),
        "a missing function is not a host the rule refused: {stderr}"
    );
    assert!(!stderr.contains("fake-sensitive-input"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "fake-existing-host-value",
        "the installed credential is unchanged"
    );
}

#[test]
fn a_rule_visudo_refuses_is_never_placed_and_root_is_never_the_grantee() {
    let dir = scratch_dir("runner-recv-install-refused");
    let out = install_runner_receiver(&dir, false, "david");
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert_eq!(
        std::fs::read_dir(dir.join("sudoers.d")).unwrap().count(),
        0,
        "a refused or staged rule was left"
    );

    let dir = scratch_dir("runner-recv-install-root");
    let out = install_runner_receiver(&dir, true, "root");
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert_eq!(std::fs::read_dir(dir.join("sudoers.d")).unwrap().count(), 0);
}

/// The converge installs the receiver only on a host the ops-runner role
/// names, and records it in exactly the words the enroll procedure tells
/// David to find before he signs (review c59facbf N1).
#[test]
fn the_converge_records_the_receiver_in_the_words_the_enroll_procedure_names() {
    let converge =
        std::fs::read_to_string(repo_root().join("infra/gcp/boss-gcp-converge.sh")).unwrap();
    let lines: Vec<&str> = converge.lines().map(str::trim).collect();
    let call = lines
        .iter()
        .position(|l| {
            !l.starts_with('#') && l.contains("gcp/install-runner-credential-receiver.sh")
        })
        .expect("the boss-gcp converge runs the runner receiver's installer");
    let opener = lines[..call]
        .iter()
        .rev()
        .find(|l| l.starts_with("if "))
        .expect("the installer runs inside a role check");
    assert_eq!(*opener, "if has_role ops-runner; then");
    let recorded = format!("run_summary_field {SUMMARY_KEY} \"{SUMMARY_INSTALLED}\"");
    assert!(
        lines[call..].iter().take(12).any(|l| *l == recorded),
        "the converge does not record `{recorded}` after the installer"
    );

    let workflow: toml::Value = toml::from_str(
        &std::fs::read_to_string(
            repo_root().join("infra/platform/workflows/prepare-a-deposit-key.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    let enroll = workflow["workflow"][0]["step"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["title"].as_str() == Some("enroll"))
        .expect("prepare-a-deposit-key has an enroll step");
    let procedure = enroll["metadata_defaults"]["procedure"].as_str().unwrap();
    let named = format!("summary records {SUMMARY_KEY} {SUMMARY_INSTALLED}");
    assert!(
        procedure.contains(&named),
        "the enroll procedure no longer sends its signer to `{named}`: {procedure}"
    );
}
