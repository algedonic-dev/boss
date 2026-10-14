//! Missing or malformed enrollment cannot trigger an off-host deposit.
use boss_testing::{repo_root, scratch_dir, write_file};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn the_sender_refuses_before_transport_when_enrollment_is_unavailable() {
    for enrollment in ["", "not-a-packet", "../scope"] {
        let out = Command::new("bash")
            .arg(repo_root().join("infra/gcp/root-tree-token-send.sh"))
            .env("BOSS_ROOT_TREE_DEPOSIT_ENROLLMENT", enrollment)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(78), "{:?}", out);
        assert!(String::from_utf8_lossy(&out.stderr).contains("enrollment"));
    }
}

#[test]
fn a_signed_transfer_records_only_after_the_exact_receiver_receipt() {
    let dir = scratch_dir("gcp-root-tree-sender-transfer");
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let adapter = bin.join("adapter.py");
    write_file(
        &adapter,
        r#"#!/usr/bin/env python3
import base64,hashlib,json,os,pathlib,sys
p=pathlib.Path(os.environ['ROOT_TREE_SENDER_FIXTURE'])
name=pathlib.Path(sys.argv[0]).name
if name=='ssh-keygen': print('ssh-ed25519 SYNTHETICFIXTURE fixture')
elif name=='kubectl':
 value=b'candidate-private-fixture-0000000000000000'
 if os.environ.get('ROOT_TREE_SENDER_MODE')=='moved-secret' and (p/'sent.json').exists(): value=b'rotated-private-fixture-111111111111111111'
 print(json.dumps({'metadata':{'uid':'22222222-2222-2222-2222-222222222222'},'data':{'token':base64.b64encode(value).decode()}}))
elif name=='curl':
 args=sys.argv[1:]
 method=args[args.index('-X')+1] if '-X' in args else 'GET'
 url=next(a for a in args if a.startswith('http'))
 if method!='GET':
  with (p/'writes').open('a') as f: f.write(method+' '+url+'\n')
 elif 'kind=rotate-a-credential' in url: print((p/'jobs.json').read_text())
 else: print((p/'enrollment.json').read_text())
elif name=='ssh':
 if os.environ.get('ROOT_TREE_SENDER_MODE')=='wrong-greeting':
  print('a shell is listening',flush=True)
  sys.exit(0)
 print('root-tree-token-recv: boss-gcp: ready for one envelope',flush=True)
 envelope=json.load(sys.stdin)
 (p/'sent.json').write_text(json.dumps(envelope))
 value=envelope.pop('value').encode()
 envelope.update(installed=True,value_sha256=hashlib.sha256(value).hexdigest(),value_length=len(value))
 if os.environ.get('ROOT_TREE_SENDER_MODE')=='wrong-receipt': envelope['secret_uid']='99999999-9999-9999-9999-999999999999'
 print(json.dumps(envelope),flush=True)
else: sys.exit(90)
"#,
    );
    // mode-bits-ok: synthetic external adapters, never installed as estate executors.
    std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["curl", "kubectl", "ssh", "ssh-keygen"] {
        std::os::unix::fs::symlink(&adapter, bin.join(name)).unwrap();
    }
    let enrollment_id = "33333333-3333-3333-3333-333333333333";
    let rotation_id = "11111111-1111-1111-1111-111111111111";
    let public = "ssh-ed25519 SYNTHETICFIXTURE fixture";
    let forced =
        "exec sudo -n /usr/local/libexec/boss/root-tree-delivery/infra/gcp/root-tree-token-recv.sh";
    let enrollment = serde_json::json!({"id":enrollment_id,"kind":"prepare-the-root-tree-deposit-key",
        "subject":{"id":"root-tree-deposit-key"},"metadata":{},"steps":[
        {"spec_slug":"install","status":"completed","metadata":{"public_key":public}},
        {"spec_slug":"enroll","status":"completed","metadata":{"decision":"approved",
        "receiver_host":"boss-gcp","purpose":"root-tree credential deposit","enrollment_record":"fixture approval",
        "authorized_keys_line":format!("command=\"{forced}\",restrict {public}")}}]});
    write_file(&dir.join("enrollment.json"), &enrollment.to_string());
    let jobs = serde_json::json!({"total":1,"data":[{"id":rotation_id,"status":"open",
        "subject":{"id":"boss-gcp-root-tree-token"},"steps":[{"id":"44444444-4444-4444-4444-444444444444",
        "spec_slug":"delivered","status":"ready"}]}]});
    write_file(&dir.join("jobs.json"), &jobs.to_string());
    write_file(&dir.join("key"), "synthetic key fixture");
    write_file(&dir.join("known"), "synthetic pin fixture");
    let estate = std::fs::read_to_string(repo_root().join("infra/estate/estate.toml")).unwrap();
    let address = estate
        .lines()
        .skip_while(|l| *l != "id = \"boss-gcp\"")
        .find_map(|l| {
            l.strip_prefix("address = \"")
                .and_then(|a| a.strip_suffix('"'))
        })
        .unwrap();
    let run = |mode: &str| {
        Command::new("bash")
            .arg(repo_root().join("infra/gcp/root-tree-token-send.sh"))
            .env("BOSS_ROOT_TREE_DEPOSIT_ENROLLMENT", enrollment_id)
            .env("BOSS_ROOT_TREE_DEPOSIT_KEY", dir.join("key"))
            .env("BOSS_ROOT_TREE_DEPOSIT_KNOWN_HOSTS", dir.join("known"))
            .env(
                "BOSS_ROOT_TREE_DEPOSIT_TARGET",
                format!("fixture@{address}"),
            )
            .env("ROOT_TREE_SENDER_FIXTURE", &dir)
            .env("ROOT_TREE_SENDER_MODE", mode)
            .env("BOSS_JOBS_URL", "http://fixture.example.invalid")
            .env("BOSS_SOR_ENV", dir.join("absent.env"))
            .env("BOSS_MACHINE_TOKEN_DIR", dir.join("no-live-token"))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap()
    };
    // A sibling protocol cannot authorize this receiver, even with otherwise
    // matching public proposal fields. Refuse before SSH or any SoR write.
    for sibling in [
        "prepare-a-deposit-key",
        "prepare-the-machine-token-deposit-key",
    ] {
        let mut wrong_protocol = enrollment.clone();
        wrong_protocol["kind"] = serde_json::json!(sibling);
        write_file(&dir.join("enrollment.json"), &wrong_protocol.to_string());
        let out = run("success");
        assert!(!out.status.success(), "accepted sibling {sibling}: {out:?}");
        assert!(!dir.join("sent.json").exists());
        assert!(!dir.join("writes").exists());
    }
    write_file(&dir.join("enrollment.json"), &enrollment.to_string());
    let out = run("success");
    assert!(out.status.success(), "{:?}", out);
    let sent: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("sent.json")).unwrap()).unwrap();
    assert_eq!(sent["job_id"], rotation_id);
    assert_eq!(sent["secret_uid"], "22222222-2222-2222-2222-222222222222");
    let writes = std::fs::read_to_string(dir.join("writes")).unwrap();
    assert_eq!(writes.lines().count(), 2);
    assert!(writes.contains("PATCH") && writes.contains("PUT"));
    let printed = [out.stdout, out.stderr].concat();
    assert!(
        !printed
            .windows(b"candidate-private-fixture".len())
            .any(|w| w == b"candidate-private-fixture")
    );
    for mode in [
        "unsigned",
        "wrong-enrollment-id",
        "wrong-greeting",
        "wrong-receipt",
        "moved-secret",
    ] {
        std::fs::remove_file(dir.join("sent.json")).unwrap();
        if dir.join("writes").exists() {
            std::fs::remove_file(dir.join("writes")).unwrap();
        }
        let mut record = enrollment.clone();
        if mode == "unsigned" {
            record["steps"][1]["status"] = serde_json::json!("ready");
        }
        if mode == "wrong-enrollment-id" {
            record["id"] = serde_json::json!("99999999-9999-9999-9999-999999999999");
        }
        write_file(&dir.join("enrollment.json"), &record.to_string());
        let result = run(mode);
        assert!(
            !dir.join("writes").exists(),
            "delivery recorded during {mode}: {:?}",
            result
        );
        if mode == "moved-secret" {
            assert!(result.status.success());
            assert!(String::from_utf8_lossy(&result.stdout).contains("Secret moved"));
        } else {
            assert!(!result.status.success(), "accepted {mode}: {:?}", result);
        }
        if mode == "unsigned" || mode == "wrong-enrollment-id" || mode == "wrong-greeting" {
            assert!(
                !dir.join("sent.json").exists(),
                "credential sent during {mode}"
            );
            // Keep cleanup independent of whether a refusal transferred bytes.
            write_file(&dir.join("sent.json"), "{}");
        }
    }
}
