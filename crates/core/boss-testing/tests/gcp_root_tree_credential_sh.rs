//! The dormant boss-gcp root-tree helper releases a synthetic credential
//! only to the declared repository, never to another git request.
use boss_testing::{feed_stdin, repo_root, scratch_dir, write_file};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

#[test]
fn only_the_exact_repository_gets_the_private_single_line_credential() {
    let dir = scratch_dir("gcp-root-tree-credential");
    // mode-bits-ok: the private credential directory, not a file anything execs.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let token = dir.join("token");
    write_file(&token, "fixture-token\n");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let run = |action: &str, input: &str| {
        let mut child = Command::new("bash")
            .arg(repo_root().join("infra/gcp/root-tree-credential.sh"))
            .arg(action)
            .env("BOSS_GCP_ROOT_TREE_TOKEN", &token)
            .env("BOSS_FORGE_URL", "http://forge.example.invalid:3000")
            .env("BOSS_SOR_ENV", dir.join("absent.env"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        feed_stdin(&mut child, input.as_bytes());
        child.wait_with_output().unwrap()
    };
    let exact = "protocol=http\nhost=forge.example.invalid:3000\npath=david/boss.git\n\n";
    let yes = run("get", exact);
    assert!(
        yes.status.success(),
        "{}",
        String::from_utf8_lossy(&yes.stderr)
    );
    assert_eq!(yes.stdout, b"username=david\npassword=fixture-token\n\n");
    for request in [
        exact.replace("http\n", "https\n"),
        exact.replace("forge.example.invalid:3000", "attacker.example.invalid"),
        exact.replace("david/boss.git", "david/other.git"),
        exact.replace("david/boss.git", "david/boss.git/extra"),
        exact.replace("path=david/boss.git\n", ""),
        format!("host=attacker.example.invalid\n{exact}"),
        format!("username=other\n{exact}"),
        exact.replace("forge.example", "forge\0.example"),
        exact.replace("boss.git", "boss\0.git"),
        exact.replace("http\n", "ht\0tp\n"),
        format!("username=da\0vid\n{exact}"),
        format!("ignored=bad\0bytes\n{exact}"),
        format!("{exact}\0"),
    ] {
        assert!(
            run("get", &request).stdout.is_empty(),
            "leaked for {request:?}"
        );
    }
    for action in ["store", "erase"] {
        assert!(run(action, exact).stdout.is_empty());
    }
    let oversized = run("get", &format!("ignored={}\n{exact}", "x".repeat(4097)));
    assert!(!oversized.status.success());
    assert!(oversized.stdout.is_empty());
    write_file(&token, &"x".repeat(4096));
    assert!(run("get", exact).status.success());
    for bad in [
        "",
        "first\nsecond\n",
        "password=injected\n",
        "with space\n",
        "fixture\0-token\n",
        "\0fixture-token\n",
        "fixture-token\0\n",
        "fixture-token\n\0",
        "fixture-token\n\n",
    ] {
        write_file(&token, bad);
        let out = run("get", exact);
        assert!(!out.status.success(), "accepted malformed token");
        assert!(out.stdout.is_empty());
    }
    write_file(&token, "fixture-token\n");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(run("get", exact).stdout.is_empty());
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    // mode-bits-ok: loosen the credential directory to exercise its refusal, not a script.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run("get", exact).stdout.is_empty());
    // mode-bits-ok: restore the private credential directory for subsequent controls, not a script.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::rename(&token, dir.join("real-token")).unwrap();
    std::os::unix::fs::symlink(dir.join("real-token"), &token).unwrap();
    assert!(run("get", exact).stdout.is_empty());
    std::fs::remove_file(&token).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(&token)
            .status()
            .unwrap()
            .success()
    );
    assert!(run("get", exact).stdout.is_empty());
    // Off-target requests return before opening a private credential,
    // even when its name is a FIFO that an accidental read would hold.
    assert!(
        run("get", &exact.replace("boss.git", "other.git"))
            .stdout
            .is_empty()
    );
    std::fs::remove_file(&token).unwrap();
    write_file(&token, &"x".repeat(4097));
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(run("get", exact).stdout.is_empty());
}
