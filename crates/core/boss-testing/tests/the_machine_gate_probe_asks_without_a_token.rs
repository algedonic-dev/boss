use std::{fs, process::Command};

#[test]
fn the_probe_measures_a_tokenless_refusal_and_preserves_transport_failure() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let script = root.join("infra/ops/machine-gate-refuses-tokenless.sh");
    let temp = boss_testing::scratch_dir("machine-gate-tokenless-probe");
    let curl = temp.as_path().join("curl");
    boss_testing::write_exec(
        &curl,
        "#!/bin/bash\nprintf '%s\\n' \"$@\" > \"$ARGS\"\nprintf '%s' \"$STATUS\"\nexit \"$CURL_EXIT\"\n",
    );
    for (status, curl_exit, expected) in [
        ("401", "0", 0),
        ("200", "0", 75),
        ("000", "7", 75),
        ("403", "0", 1),
    ] {
        let args = temp.as_path().join("args");
        let output = Command::new("/bin/bash")
            .arg(&script)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", temp.as_path().display()),
            )
            .env("BOSS_JOBS_URL", "http://fixture.invalid:7900")
            .env("BOSS_MACHINE_TOKEN", "must-not-be-sent")
            .env("STATUS", status)
            .env("CURL_EXIT", curl_exit)
            .env("ARGS", &args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let sent = fs::read_to_string(args).unwrap();
        assert!(!sent.contains("must-not-be-sent"));
        assert!(sent.starts_with("--disable\n"));
        assert!(!sent.contains("x-boss-machine-token"));
        let user = sent
            .lines()
            .find_map(|l| l.strip_prefix("x-boss-user: "))
            .unwrap();
        let parsed: boss_policy_client::User = serde_json::from_str(user).unwrap();
        assert_eq!(parsed.id, "machine-gate-tokenless-probe");
        assert!(sent.contains("http://fixture.invalid:7900/api/jobs?limit=1"));
    }
}
