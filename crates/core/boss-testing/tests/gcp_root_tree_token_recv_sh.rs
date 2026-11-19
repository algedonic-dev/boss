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
    // The failure names its check and the status the forge answered, and
    // says nothing was replaced (backlog fd4fbe49): curl's own failure and
    // a non-200 answer both read `repository-authentication` until then.
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "root-tree-token-recv: failed (repository-authentication-http-401); installed credential unchanged; delivery not acknowledged"
        ),
        "{:?}",
        out
    );
    let printed = [out.stdout, out.stderr].concat();
    assert!(!printed.windows(value.len()).any(|v| v == value.as_bytes()));
}

/// A curl stub standing in for the forge: it answers the repository API
/// 200 only when the token arrives through a private header file, and
/// exits nonzero if the value ever reaches its argv.
const AUTHENTICATED_CURL: &str = r#"#!/bin/sh
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

/// Runs one well-formed delivery against a forge that authenticates it,
/// with `stubs` added to (or replacing) the fixture's PATH executables,
/// so a single host act can be planted to fail. Returns the output and
/// the destination the receiver wrote, which starts as `old-private-fixture`.
fn deliver_with(name: &str, stubs: &[(&str, &str)]) -> (std::process::Output, std::path::PathBuf) {
    let dir = scratch_dir(name);
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
    let git = "#!/bin/sh\nprintf '0123456789012345678901234567890123456789\\trefs/heads/main\\n'\n";
    for (name, script) in [("git", git), ("curl", AUTHENTICATED_CURL)]
        .iter()
        .chain(stubs.iter())
    {
        write_exec(&bin.join(name), script);
    }
    let envelope = serde_json::json!({"version":1,"host":"boss-gcp",
        "credential_id":"boss-gcp-root-tree-token",
        "job_id":"11111111-1111-1111-1111-111111111111",
        "secret_uid":"22222222-2222-2222-2222-222222222222",
        "value":"candidate-private-fixture-0000000000000000"});
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
    (child.wait_with_output().unwrap(), token)
}

/// INSTALLED FLIPS AT THE RENAME, NOT BEFORE IT (backlog fd4fbe49, the
/// lesson a61cce40 taught the runner receiver). The rename is the one act
/// that replaces the forge token; planted to fail here with a PATH `mv`
/// that refuses, every check ahead of it has passed, so a flip placed
/// anywhere before the rename would tell the operator the delivered token
/// is installed with the old one still in place.
#[test]
fn a_failed_rename_says_the_installed_credential_is_unchanged() {
    let (out, token) = deliver_with(
        "gcp-root-tree-token-rename",
        &[("mv", "#!/bin/sh\nexit 1\n")],
    );
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "root-tree-token-recv: failed (installation); installed credential unchanged; delivery not acknowledged"
        ),
        "{:?}",
        out
    );
    assert_eq!(std::fs::read(&token).unwrap(), b"old-private-fixture");
}

/// A FAILURE AFTER THE RENAME SAYS THE TOKEN IS INSTALLED (backlog
/// fd4fbe49, from review run e5d18320 of car 7b2daccc). The line said
/// only "delivery not acknowledged", so a host whose forge token had been
/// replaced and never acknowledged read exactly like one where nothing
/// changed. Planted with a PATH `sync` that syncs a file and refuses a
/// directory: the staged file passes, the rename lands, and the directory
/// sync after it fails.
#[test]
fn a_failure_after_the_rename_says_the_delivered_credential_is_installed() {
    let sync = "#!/bin/sh\nfor a; do [ -d \"$a\" ] && exit 1; done\nexit 0\n";
    let (out, token) = deliver_with("gcp-root-tree-token-dir-sync", &[("sync", sync)]);
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "root-tree-token-recv: failed (directory-sync); the delivered credential IS installed; delivery not acknowledged"
        ),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read(&token).unwrap(),
        b"candidate-private-fixture-0000000000000000"
    );
    let printed = [out.stdout.clone(), out.stderr.clone()].concat();
    assert!(
        !printed
            .windows(b"candidate-private-fixture".len())
            .any(|v| v == b"candidate-private-fixture")
    );
}

/// EVERY REFUSAL AND EVERY FAILURE NAMES ITS OWN CHECK (backlog fd4fbe49;
/// the scan car 7b2daccc gave runner-credential-recv.sh, read off this
/// receiver). No `refuse` or `fail` call is bare, and no two share a
/// word: `document` stood for two checks, `staging` for two and
/// `repository-authentication` for two, so a recorded failure did not say
/// which check answered. A call that opens a line (an indented `fail`
/// inside an `if`) counts as well as one behind `||`.
#[test]
fn every_refusal_and_every_failure_in_the_root_tree_receiver_names_its_own_check() {
    let body =
        std::fs::read_to_string(repo_root().join("infra/gcp/root-tree-token-recv.sh")).unwrap();
    let mut words: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (verb, floor) in [("refuse", 16), ("fail", 16)] {
        let call = regex::Regex::new(&format!(
            r"(^\s*|[|&;{{(]\s*){verb}(?P<rest>([\s;}}|&)].*)?)$"
        ))
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
    for (name, script) in [
        (
            "git",
            "#!/bin/sh\nprintf '0123456789012345678901234567890123456789\\trefs/heads/main\\n'\n",
        ),
        ("curl", AUTHENTICATED_CURL),
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
