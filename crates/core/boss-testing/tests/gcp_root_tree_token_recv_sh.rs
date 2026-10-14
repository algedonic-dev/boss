//! Root's Forgejo token deposit consumes one bounded envelope; bad input
//! must never replace the existing private credential or print its bytes.
use boss_testing::{feed_stdin, repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

#[test]
fn malformed_root_tree_envelopes_keep_the_installed_credential() {
    let dir = scratch_dir("gcp-root-tree-token-recv");
    // mode-bits-ok: private fixture directory, not an executable file.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let token = dir.join("forge.token");
    write_file(&token, "existing-private-fixture\n");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    for input in [
        Vec::new(),
        b"candidate-private-fixture\n".to_vec(),
        b"{\"value\":\"candidate-private-fixture\"}".to_vec(),
        b"{\"value\":\"candidate\0private-fixture\"}".to_vec(),
        vec![b'x'; 8193],
    ] {
        let mut child = Command::new("bash")
            .arg(repo_root().join("infra/gcp/root-tree-token-recv.sh"))
            .env("BOSS_GCP_ROOT_TREE_TOKEN", &token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        feed_stdin(&mut child, &input);
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(78), "{:?}", out);
        assert_eq!(
            std::fs::read(&token).unwrap(),
            b"existing-private-fixture\n"
        );
        let printed = [out.stdout, out.stderr].concat();
        assert!(
            !printed
                .windows(b"private-fixture".len())
                .any(|v| v == b"private-fixture")
        );
    }
}

#[test]
fn an_anonymous_git_read_cannot_substitute_for_token_authentication() {
    let dir = scratch_dir("gcp-root-tree-token-authentication");
    // mode-bits-ok: private fixture directory, not an executable file.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let token = dir.join("forge.token");
    write_file(&token, "existing-private-fixture\n");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    // The external Forgejo adapter is unavailable in this domain test.
    // Git can read a public repository, while authenticated HTTP refuses.
    for (name, script) in [
        (
            "git",
            "#!/bin/sh\nprintf '0123456789012345678901234567890123456789\\trefs/heads/main\\n'\n",
        ),
        ("curl", "#!/bin/sh\nprintf '401'\n"),
    ] {
        let path = bin.join(name);
        write_exec(&path, script);
    }
    let value = "candidate-private-fixture-0000000000000000";
    let envelope = serde_json::json!({"version":1,"host":"boss-gcp",
        "credential_id":"boss-gcp-root-tree-token",
        "job_id":"11111111-1111-1111-1111-111111111111",
        "secret_uid":"22222222-2222-2222-2222-222222222222","value":value});
    let mut child = Command::new("bash")
        .arg(repo_root().join("infra/gcp/root-tree-token-recv.sh"))
        .env("BOSS_GCP_ROOT_TREE_TOKEN", &token)
        .env("BOSS_FORGE_URL", "http://forge.example.invalid:3000")
        .env("BOSS_SOR_ENV", dir.join("absent.env"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, envelope.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    assert_eq!(
        std::fs::read(&token).unwrap(),
        b"existing-private-fixture\n"
    );
    let printed = [out.stdout, out.stderr].concat();
    assert!(!printed.windows(value.len()).any(|v| v == value.as_bytes()));
}

#[test]
fn successful_delivery_uses_a_private_header_and_returns_only_bound_evidence() {
    let dir = scratch_dir("gcp-root-tree-token-success");
    // mode-bits-ok: private fixture directory, not an executable file.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let private = dir.join("private");
    std::fs::create_dir(&private).unwrap();
    // mode-bits-ok: private fixture directory, not an executable file.
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    let token = private.join("forge.token");
    write_file(&token, "old-private-fixture");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let value = "candidate-private-fixture-0000000000000000";
    let curl = r#"#!/bin/sh
output='' header=''
while [ $# -gt 0 ]; do
  case "$1" in
    *candidate-private-fixture*) exit 91;;
    -o) output=$2; shift;;
    -H) header=$2; shift;;
  esac
  shift
done
case "$header" in @*) header=${header#@};; *) exit 92;; esac
[ "$(stat -c %a "$header")" = 600 ] || exit 93
[ "$(cat "$header")" = 'Authorization: token candidate-private-fixture-0000000000000000' ] || exit 94
printf '{"full_name":"david/boss"}' > "$output"
printf '200'
"#;
    for (name, script) in [
        (
            "git",
            "#!/bin/sh\nprintf '0123456789012345678901234567890123456789\\trefs/heads/main\\n'\n",
        ),
        ("curl", curl),
    ] {
        let path = bin.join(name);
        write_exec(&path, script);
    }
    let envelope = serde_json::json!({"version":1,"host":"boss-gcp",
        "credential_id":"boss-gcp-root-tree-token",
        "job_id":"11111111-1111-1111-1111-111111111111",
        "secret_uid":"22222222-2222-2222-2222-222222222222","value":value});
    let mut child = Command::new("bash")
        .arg(repo_root().join("infra/gcp/root-tree-token-recv.sh"))
        .env("BOSS_GCP_ROOT_TREE_TOKEN", &token)
        .env("BOSS_FORGE_URL", "http://forge.example.invalid:3000")
        .env("BOSS_SOR_ENV", dir.join("absent.env"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, envelope.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(std::fs::read(&token).unwrap(), value.as_bytes());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let receipt: serde_json::Value = serde_json::from_str(stdout.lines().nth(1).unwrap()).unwrap();
    assert_eq!(receipt["job_id"], envelope["job_id"]);
    assert_eq!(receipt["secret_uid"], envelope["secret_uid"]);
    assert_eq!(receipt["value_length"], value.len());
    assert_eq!(receipt["installed"], true);
    assert!(receipt.get("value").is_none());
    assert!(!stdout.contains(value));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(value));
    assert_eq!(
        std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // A private leaf can still be replaced through its writable ancestor.
    // mode-bits-ok: hostile fixture directory, never an executable file.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let mut child = Command::new("bash")
        .arg(repo_root().join("infra/gcp/root-tree-token-recv.sh"))
        .env("BOSS_GCP_ROOT_TREE_TOKEN", &token)
        .env("BOSS_FORGE_URL", "http://forge.example.invalid:3000")
        .env("BOSS_SOR_ENV", dir.join("absent.env"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, envelope.to_string().as_bytes());
    let refused = child.wait_with_output().unwrap();
    assert_eq!(refused.status.code(), Some(78), "{:?}", refused);
}
