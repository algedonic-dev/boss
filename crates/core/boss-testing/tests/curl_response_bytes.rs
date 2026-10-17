//! The shared roll-wait door conserves bytes, not Bash strings (6cc6d770).
use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::process::{Command, Output};

const PAYLOAD: &[u8] = b"A\0B\xff\n\n";

fn run(tag: &str, body: &str, args: &[&str]) -> (Output, std::path::PathBuf) {
    let root = scratch_dir(&format!("curl-bytes-{tag}"));
    let bin = root.join("bin");
    let temp = root.join("temp");
    create_dir(&bin);
    create_dir(&temp);
    std::fs::write(root.join("payload"), PAYLOAD).unwrap();
    write_exec(&bin.join("curl"), body);
    write_exec(
        &bin.join("sleep"),
        "#!/bin/bash\nprintf '%s\\n' \"$1\" >> \"$SLEEPS\"\n",
    );
    let out = Command::new("bash")
        .args(["-c", ". \"$LIB\"; curl_through_a_roll \"$@\"", "bytes"])
        .args(args)
        .env("LIB", repo_root().join("infra/lib/curl-through-a-roll.sh"))
        .env("PAYLOAD", root.join("payload"))
        .env("CALLS", root.join("calls"))
        .env("SLEEPS", root.join("sleeps"))
        .env("TMPDIR", &temp)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert_eq!(
        std::fs::read_dir(temp).unwrap().count(),
        0,
        "response spool leaked"
    );
    (out, root)
}

#[test]
fn binary_stdout_and_trailing_newlines_survive() {
    let (out, _) = run(
        "binary",
        "#!/bin/bash\ncat \"$PAYLOAD\"\n",
        &["0", "door", "API", "GET bytes"],
    );
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(out.stdout, PAYLOAD);
    assert!(out.stderr.is_empty(), "{:?}", out);
}

#[test]
fn text_stdout_keeps_its_terminal_newlines() {
    let (out, _) = run(
        "text",
        "#!/bin/bash\nprintf 'answer\\n\\n'\n",
        &["0", "door", "API", "GET text"],
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"answer\n\n");
}

#[test]
fn ambiguous_post_failures_keep_partial_bytes_and_are_never_replayed() {
    for code in [22, 23, 28, 35, 52, 55, 56] {
        let body = format!(
            "#!/bin/bash\nprintf '%s\\n' \"$*\" >> \"$CALLS\"\ncat \"$PAYLOAD\"\nexit {code}\n"
        );
        let (out, root) = run(
            &format!("post-{code}"),
            &body,
            &["30", "door", "API", "POST bytes", "-X", "POST", "-d", "{}"],
        );
        assert_eq!(out.status.code(), Some(code));
        assert_eq!(out.stdout, PAYLOAD, "partial output for curl {code}");
        assert_eq!(
            std::fs::read_to_string(root.join("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(!root.join("sleeps").exists());
    }
}

#[test]
fn only_refused_connects_retry_and_discard_the_refusal_stdout() {
    for code in [6, 7] {
        let body = format!(
            "#!/bin/bash\nif [ ! -e \"$CALLS\" ]; then touch \"$CALLS\"; printf '000'; exit {code}; fi\ncat \"$PAYLOAD\"\n"
        );
        let (out, root) = run(
            &format!("retry-{code}"),
            &body,
            &["30", "door", "API", "POST bytes", "-X", "POST"],
        );
        assert!(out.status.success(), "{:?}", out);
        assert_eq!(out.stdout, PAYLOAD);
        assert_eq!(std::fs::read_to_string(root.join("sleeps")).unwrap(), "2\n");
    }
}

#[test]
fn expired_refusals_have_no_stdout() {
    for code in [6, 7] {
        let (out, _) = run(
            &format!("expired-{code}"),
            &format!("#!/bin/bash\nprintf '000'; exit {code}\n"),
            &["0", "door", "API", "GET bytes"],
        );
        assert_eq!(out.status.code(), Some(code));
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn real_curl_output_file_and_write_out_keep_their_separate_bytes() {
    let root = scratch_dir("curl-real-output");
    let payload = root.join("payload");
    let destination = root.join("destination");
    create_dir(&root);
    std::fs::write(&payload, PAYLOAD).unwrap();
    let out = Command::new("bash")
        .args(["-c", ". \"$LIB\"; curl_through_a_roll 0 door file GET -sS -o \"$DEST\" -w 'status:%{http_code}\\n\\n' \"$URL\""])
        .env("LIB", repo_root().join("infra/lib/curl-through-a-roll.sh"))
        .env("DEST", &destination)
        .env("URL", format!("file://{}", payload.display()))
        .output().unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(std::fs::read(destination).unwrap(), PAYLOAD);
    assert_eq!(out.stdout, b"status:000\n\n");
}

#[test]
fn spool_refusal_does_not_send_and_leaves_caller_traps_intact() {
    let out = Command::new("bash")
        .args([
            "-c",
            r#"
. "$LIB"
mktemp() { return 73; }
curl() { printf sent; }
trap 'printf caller-exit' EXIT
curl_through_a_roll 0 door API POST
rc=$?
printf 'status:%s\n' "$rc"
exit "$rc"
"#,
        ])
        .env("LIB", repo_root().join("infra/lib/curl-through-a-roll.sh"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(73));
    assert_eq!(out.stdout, b"status:73\ncaller-exit");
}

#[test]
fn a_failed_stdout_copy_cannot_report_success() {
    let out = Command::new("bash")
        .args([
            "-c",
            r#"
. "$LIB"
curl() { printf response; }
cat() { return 74; }
curl_through_a_roll 0 door API GET
"#,
        ])
        .env("LIB", repo_root().join("infra/lib/curl-through-a-roll.sh"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(74));
    assert!(out.stdout.is_empty());
}
