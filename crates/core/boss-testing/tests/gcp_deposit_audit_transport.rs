//! Immutable audit code crosses the account boundary as bytes, not a private path.
// not a tree-wide pin: owns the GCP deposit-account audit transport.
use boss_testing::{copy_exec, create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn transport() -> String {
    let source =
        std::fs::read_to_string(repo_root().join("infra/gcp/boss-gcp-converge.sh")).unwrap();
    let start = source.find("audit_script=").unwrap();
    let end = source[start..].find("sed 's/^/  deposit keys: /'").unwrap() + start;
    source[start..end].to_owned()
}

fn run(dir: &Path, account: &str, keys: &Path, real_user: bool, bound: u32) -> Output {
    let bin = dir.join("bin");
    create_dir(&bin);
    if !real_user {
        // CI is itself unprivileged. Exercise its same command boundary;
        // the root-only control below separately measures an actual uid drop.
        write_exec(
            &bin.join("runuser"),
            "#!/bin/bash\n[ \"$1\" = -u ] && [ \"$3\" = -- ] || exit 99\nshift 3\nexec \"$@\"\n",
        );
    }
    let runner = dir.join("transport.sh");
    write_exec(
        &runner,
        &format!(
            "#!/bin/bash\nset -uo pipefail\naudit_bound=$BOUND\nDEPOSIT_ACCOUNT=$ACCOUNT\nak_files=(\"$KEYS\")\nalog=$LOG\narc=0\n{}\nprintf 'audit-exit=%s\\n' \"$arc\"\ncat \"$alog\"\nexit \"$arc\"\n",
            transport()
        ),
    );
    Command::new("bash")
        .arg(&runner)
        .env("BOUND", bound.to_string())
        .env("ACCOUNT", account)
        .env("KEYS", keys)
        .env("LOG", dir.join("audit.log"))
        .env("BOSS_GCP_CONVERGE_INFRA", dir.join("private/infra"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap()
}

fn fixture() -> std::path::PathBuf {
    let dir = scratch_dir("gcp-audit-private-transport");
    let private = dir.join("private/infra/gcp");
    create_dir(&private);
    copy_exec(
        &repo_root().join("infra/gcp/audit-deposit-keys.sh"),
        &private.join("audit-deposit-keys.sh"),
    );
    // mode-bits-ok: a private fixture directory, not an executable file.
    std::fs::set_permissions(dir.join("private"), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn confined_keys(dir: &Path) -> std::path::PathBuf {
    let key = dir.join("fixture-key");
    let generated = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(generated.status.success(), "{generated:?}");
    let public = std::fs::read_to_string(dir.join("fixture-key.pub")).unwrap();
    let keys = dir.join("authorized_keys");
    write_file(
        &keys,
        &format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict {}\n",
            public.trim()
        ),
    );
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o644)).unwrap();
    keys
}

fn assert_clean(out: &Output) {
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("deposit-key-audit: clean"),
        "{out:?}"
    );
}

fn stat_stopped(stat: &str) -> Result<bool, String> {
    let (_, tail) = stat.rsplit_once(") ").ok_or("malformed procfs stat")?;
    match tail.split_whitespace().next() {
        Some("Z" | "X" | "x") => Ok(true),
        Some("R" | "S" | "D" | "T" | "t" | "I" | "K" | "W" | "P") => Ok(false),
        state => Err(format!("unknown procfs process state {state:?}: {stat}")),
    }
}

#[test]
fn procfs_evidence_refuses_unknown_and_retains_live_states() {
    assert_eq!(stat_stopped("123 (sleep) Z 1 2"), Ok(true));
    assert_eq!(stat_stopped("123 (name with ) inside) S 1 2"), Ok(false));
    assert_eq!(stat_stopped("123 (sleep) T 1 2"), Ok(false));
    assert!(stat_stopped("").is_err());
    assert!(stat_stopped("123 (sleep) ? 1 2").is_err());
}

#[test]
fn a_distinct_real_account_audits_private_code_without_traversal() {
    let uid = Command::new("id").arg("-u").output().unwrap();
    if uid.stdout != b"0\n" {
        eprintln!(
            "NOT RUN: real distinct-user control requires root; unprivileged transport controls run separately"
        );
        return;
    }
    let dir = fixture();
    let keys = confined_keys(&dir);
    let child_uid = Command::new("runuser")
        .args(["-u", "nobody", "--", "id", "-u"])
        .output()
        .unwrap();
    assert!(child_uid.status.success(), "{child_uid:?}");
    assert_ne!(child_uid.stdout, uid.stdout);
    let unreadable_source = Command::new("runuser")
        .args(["-u", "nobody", "--", "bash"])
        .arg(dir.join("private/infra/gcp/audit-deposit-keys.sh"))
        .arg(&keys)
        .output()
        .unwrap();
    assert_eq!(
        unreadable_source.status.code(),
        Some(126),
        "{unreadable_source:?}"
    );
    assert_clean(&run(&dir, "nobody", &keys, true, 5));
    assert_eq!(
        std::fs::metadata(dir.join("private"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o600)).unwrap();
    let unreadable = run(&dir, "nobody", &keys, true, 5);
    assert_eq!(unreadable.status.code(), Some(2), "{unreadable:?}");
    assert!(String::from_utf8_lossy(&unreadable.stdout).contains("UNVERIFIED"));
}

#[test]
fn the_account_child_consumes_code_from_stdin_and_keeps_audit_verdicts() {
    let dir = fixture();
    let keys = confined_keys(&dir);
    // A child sees the account-shaped command rather than a usable source
    // pathname. This catches the seam in the unprivileged full gate too.
    let bin = dir.join("bin");
    create_dir(&bin);
    let out = run(&dir, "fixture-other", &keys, false, 5);
    assert_clean(&out);
    let source = transport();
    assert!(
        source.contains("bash -s --"),
        "private code must be preopened before the account child: {source}"
    );
    let bad = dir.join("nul_keys");
    write_file(&bad, "not-a-key\0\n");
    let invalid = run(&dir, "fixture-other", &bad, false, 5);
    assert_eq!(invalid.status.code(), Some(2), "{invalid:?}");
    assert!(String::from_utf8_lossy(&invalid.stdout).contains("UNVERIFIED"));
    let public = std::fs::read_to_string(dir.join("fixture-key.pub")).unwrap();
    write_file(
        &keys,
        &format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\" {}\n",
            public.trim()
        ),
    );
    let finding = run(&dir, "fixture-other", &keys, false, 5);
    assert_eq!(finding.status.code(), Some(1), "{finding:?}");
    assert!(String::from_utf8_lossy(&finding.stdout).contains("UNCONFINED"));
}

#[test]
fn same_account_still_reads_code_and_a_missing_source_cannot_pass() {
    let dir = fixture();
    let keys = confined_keys(&dir);
    let account = Command::new("id").arg("-un").output().unwrap();
    let account = String::from_utf8(account.stdout).unwrap();
    assert_clean(&run(&dir, account.trim(), &keys, false, 5));
    std::fs::remove_file(dir.join("private/infra/gcp/audit-deposit-keys.sh")).unwrap();
    let missing = run(&dir, "fixture-other", &keys, false, 5);
    assert!(!missing.status.success(), "{missing:?}");
    assert!(!String::from_utf8_lossy(&missing.stdout).contains("deposit-key-audit: clean"));
    assert!(
        String::from_utf8_lossy(&missing.stdout).contains("No such file or directory"),
        "the captured audit log must retain the actual source refusal: {missing:?}"
    );
}

#[test]
fn the_bound_stays_inside_the_account_child_and_stops_descendants() {
    let dir = fixture();
    let keys = confined_keys(&dir);
    let pid = dir.join("child.pid");
    write_file(&pid, "");
    std::fs::set_permissions(&pid, std::fs::Permissions::from_mode(0o666)).unwrap();
    // A controlled delayed-start case showed a one-second clock could
    // expire before bash started. That was not the native gate failure:
    // both gates failed spawning absent ps. Keep the independent start
    // receipt improvement and read procfs without that extra tool. Delay startup
    // deliberately and signal GNU timeout's own alarm handler only AFTER
    // the real child has recorded its descendant. Keep the production bound;
    // neither sleeping through a guessed launch time nor a longer guess
    // can replace this start receipt (4d7465d2, gate8777f02c).
    write_exec(
        &dir.join("private/infra/gcp/audit-deposit-keys.sh"),
        &format!(
            "#!/bin/bash\nsleep 2\nsleep 30 &\nchild=$!\nprintf '%s\\n' \"$child\" > '{}' || exit 97\nkill -ALRM \"$PPID\" || exit 98\nwait \"$child\"\n",
            pid.display()
        ),
    );
    let uid = Command::new("id").arg("-u").output().unwrap();
    let real_user = uid.stdout == b"0\n";
    let account = if real_user { "nobody" } else { "fixture-other" };
    let out = run(&dir, account, &keys, real_user, 60);
    assert_eq!(out.status.code(), Some(124), "{out:?}");
    let child = std::fs::read_to_string(&pid).unwrap_or_else(|e| {
        panic!(
            "missing descendant-start receipt {}: {e}; {out:?}",
            pid.display()
        )
    });
    let child = child.trim();
    assert!(
        !child.is_empty() && child.bytes().all(|b| b.is_ascii_digit()),
        "invalid descendant-start receipt {child:?}; {out:?}"
    );
    // No procps package is in the gate image. A missing/unreadable procfs
    // must not turn every absent path into a termination pass.
    let control = std::fs::read_to_string("/proc/self/stat")
        .expect("the same procfs must answer a known process before absence counts");
    assert_eq!(
        stat_stopped(&control),
        Ok(false),
        "the known control must be alive: {control}"
    );
    let path = Path::new("/proc").join(child).join("stat");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (stopped, standing) = match std::fs::read_to_string(&path) {
            Ok(stat) => (stat_stopped(&stat).unwrap_or_else(|e| panic!("{e}")), stat),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (true, "gone".to_owned()),
            Err(e) => panic!("cannot judge descendant {}: {e}", path.display()),
        };
        if stopped {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed-out descendant {child} still executing: {standing}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
