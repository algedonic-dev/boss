//! The one-stick recovery kit (backlog c1bb822e; David's decisions
//! 2026-09-29: an ordinary stick, gpg symmetric encryption, the passphrase
//! his alone and typed only into gpg's own prompt, the Talos secrets
//! bundle derived by BOSS itself because nobody holds it). Four files,
//! each driven here:
//!
//! * `infra/forge/recovery-kit.sh` — `--assemble` (the ops verb
//!   cut-recovery-kit) and `--discard`, as root on the forge;
//! * `infra/forge/recovery-kit-read.sh` — the root-owned reader a
//!   workstation reaches through its one sudoers rule;
//! * `infra/forge/write-recovery-kit.sh` — the writer, run on any machine
//!   on the estate's tunnel, from its own checkout;
//! * `infra/forge/install-recovery-kit-reader.sh` — installs the reader
//!   outside the checkout and renders the sudoers rule.
//!
//! RUN against stubs, the way the other forge scripts are tested
//! (forge_backup_sh.rs): `findmnt`/`mount`/`umount`/`swapon` stand in for
//! the tmpfs, `docker` and `df` for the forge dump (the REAL
//! forge-backup.sh, so its verification runs here too), `systemctl`,
//! `kubectl` and `talosctl` for the host and the cluster. On the writing
//! side `ssh` execs what it is handed (or, for a forced-command key, what
//! the forced command would), `sudo` runs only the installed reader's
//! path — mapped to the tree's reader, so a writer that named anything
//! else fails — `gpg` is the identity cipher, and `udisksctl`/`sync`
//! record the re-mount the read-back must come after.
//!
//! Every credential fixture carries a `fake-…` sentinel, and every run
//! asserts that no sentinel reaches what it prints: the verb's output is
//! recorded on the ops-request packet, which everyone with the queue can
//! read. The adversarial review of car 3d12b774 (HOLD: 1 high, 4 medium,
//! 4 low) is why the reader, the installer, the re-mount, completeness,
//! the partial-kit refusal and the discard are each pinned below.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use boss_testing::{create_dir, feed_stdin, repo_root, scratch_dir, write_exec, write_file};

const SCRIPT: &str = "infra/forge/recovery-kit.sh";
const READER_SRC: &str = "infra/forge/recovery-kit-read.sh";
const WRITER: &str = "infra/forge/write-recovery-kit.sh";
const INSTALLER: &str = "infra/forge/install-recovery-kit-reader.sh";
/// Where the reader is installed and the only path sudo grants.
const READER: &str = "/usr/local/libexec/boss/recovery-kit-read";
const REQUEST: &str = "0f0e0d0c-0b0a-4908-8706-050403020100";

/// One sentinel per credential the kit carries. Short, and each says
/// `fake` — they are fixtures, and must read as such to the secret lint.
const SENTINELS: [&str; 9] = [
    "fake-github-token-7f3a",
    "fake-talosconfig-key-7f3a",
    "fake-kubeconfig-key-7f3a",
    "fake-runner-token-7f3a",
    "fake-sudoers-line-7f3a",
    "fake-gcs-key-7f3a",
    "fake-talos-bundle-7f3a",
    "fake-machine-config-7f3a",
    "fake-forge-jwt-7f3a",
];

/// The two certificate authorities, as the cluster holds them. The kit
/// proves the derived bundle by comparing ITS CAs to these.
const K8S_CA: &str = "fake k8s ca\n";
const OS_CA: &str = "fake talos os ca\n";

struct Opts {
    github_token: bool,
    sudoers: bool,
    gcs_secret: bool,
    /// The os CA the derived bundle carries — `OS_CA` when it is this
    /// cluster's.
    derived_os_ca: &'static str,
    /// What `findmnt` says the kit directory is once mounted.
    fstype: &'static str,
    /// Whether `talosctl read` hands back the state file; when it does
    /// not, the kit falls back to `get machineconfig -o yaml`.
    talos_read_works: bool,
    /// Whether this kernel accepts tmpfs `noswap`.
    noswap_ok: bool,
    /// Whether `swapon` lists a swap device.
    swap_on: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            github_token: true,
            sudoers: true,
            gcs_secret: true,
            derived_os_ca: OS_CA,
            fstype: "tmpfs",
            talos_read_works: true,
            noswap_ok: true,
            swap_on: false,
        }
    }
}

struct Case {
    dir: PathBuf,
    bin: PathBuf,
    kit: PathBuf,
    calls: PathBuf,
    stick: PathBuf,
    log: PathBuf,
    patches: PathBuf,
}

/// `<tool> <args>` over `input`, its stdout. Both tools read to EOF
/// before they write, so feeding first cannot deadlock.
fn filter(tool: &str, args: &[&str], input: &[u8]) -> String {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {tool}: {e}"));
    feed_stdin(&mut child, input);
    let out = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("{tool} output: {e}"));
    assert!(out.status.success(), "{tool} failed");
    String::from_utf8(out.stdout).expect("ascii")
}

fn b64(s: &str) -> String {
    filter("base64", &["-w0"], s.as_bytes())
}

fn sha256_of(bytes: &[u8]) -> String {
    filter("sha256sum", &[], bytes)[..64].to_string()
}

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().expect("id -u");
    String::from_utf8(out.stdout)
        .expect("uid")
        .trim()
        .to_string()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn case(name: &str, o: Opts) -> Case {
    let dir = scratch_dir(&format!("recovery-kit-{name}"));
    let bin = dir.join("bin");
    let state = dir.join("state");
    let calls = dir.join("calls");
    let stick = dir.join("stick");
    for d in [
        &bin,
        &state,
        &stick,
        &dir.join("run"),
        &dir.join("ops"),
        &dir.join("publish"),
    ] {
        create_dir(d);
    }
    let kit = dir.join("run/boss-recovery-kit");

    // --- the tmpfs, and the stick's mount --------------------------------
    // `findmnt --target` is the writer asking about the stick; anything
    // else is the kit's mount point.
    write_exec(
        &bin.join("findmnt"),
        &format!(
            "#!/usr/bin/env bash\ncase \" $* \" in\n\
             *' --target '*) case \" $* \" in *' SOURCE '*) echo /dev/fakestick ;; *) echo \"$BOSS_TEST_STICK\" ;; esac ;;\n\
             *) [ -e {s}/mounted ] && echo {fs} ;;\n\
             esac\nexit 0\n",
            s = state.display(),
            fs = o.fstype
        ),
    );
    write_exec(
        &bin.join("mount"),
        &format!(
            "#!/usr/bin/env bash\nprintf 'mount %s\\n' \"$*\" >>{c}\n\
             case \"$*\" in *noswap*) [ {ok} = 1 ] || {{ echo 'mount: tmpfs: bad option noswap' >&2; exit 32; }} ;; esac\n\
             touch {s}/mounted\n",
            c = calls.display(),
            s = state.display(),
            ok = u8::from(o.noswap_ok)
        ),
    );
    write_exec(
        &bin.join("umount"),
        &format!(
            "#!/usr/bin/env bash\nprintf 'umount %s\\n' \"$*\" >>{c}\n\
             [ \"${{BOSS_TEST_UMOUNT_FAIL:-0}}\" = 1 ] && {{ echo 'umount: target is busy' >&2; exit 32; }}\n\
             rm -f {s}/mounted\n",
            c = calls.display(),
            s = state.display()
        ),
    );
    write_exec(
        &bin.join("swapon"),
        if o.swap_on {
            "#!/usr/bin/env bash\necho /dev/fakeswap\n"
        } else {
            "#!/usr/bin/env bash\nexit 0\n"
        },
    );

    // --- the forge dump (forge-backup.sh's own stubs) ---------------------
    let tree = dir.join("forgejo");
    for d in ["repos/david/boss.git", "data/jwt"] {
        create_dir(&tree.join(d));
    }
    write_file(
        &tree.join("forgejo-db.sql"),
        "CREATE TABLE user (id INTEGER);\n",
    );
    write_file(&tree.join("app.ini"), "[server]\n");
    write_file(
        &tree.join("repos/david/boss.git/HEAD"),
        "ref: refs/heads/main\n",
    );
    write_file(&tree.join("data/jwt/private.pem"), "fake-forge-jwt-7f3a\n");
    let fixture = dir.join("dump.tar.gz");
    let st = Command::new("tar")
        .arg("-czf")
        .arg(&fixture)
        .arg("-C")
        .arg(&tree)
        .args(["forgejo-db.sql", "app.ini", "repos", "data"])
        .status()
        .expect("tar the dump fixture");
    assert!(st.success());
    write_exec(
        &bin.join("docker"),
        &format!(
            "#!/usr/bin/env bash\ncase \"$1\" in\ninspect) echo true ;;\nexec) cat {f} ;;\nesac\n",
            f = fixture.display()
        ),
    );
    write_exec(
        &bin.join("df"),
        "#!/usr/bin/env bash\necho 'Filesystem 1K-blocks Used Available Use% Mounted on'\n\
         echo \"tmpfs 4194304 0 4194304 0% /run/boss-recovery-kit\"\n",
    );

    // --- the forge host's untreed secrets ---------------------------------
    if o.github_token {
        write_file(
            &dir.join("publish/github.token"),
            "fake-github-token-7f3a\n",
        );
    }
    write_file(
        &dir.join("ops/talosconfig"),
        &format!(
            "context: boss\ncontexts:\n    boss:\n        endpoints:\n            - 192.0.2.11\n        ca: {}\n        key: fake-talosconfig-key-7f3a\n",
            b64(OS_CA)
        ),
    );
    write_file(
        &dir.join("ops/kubeconfig"),
        &format!(
            "apiVersion: v1\nclusters:\n    - cluster:\n        certificate-authority-data: {}\n        server: https://192.0.2.11:6443\n      name: boss\nusers:\n    - name: admin\n      user:\n        client-key-data: fake-kubeconfig-key-7f3a\n",
            b64(K8S_CA)
        ),
    );
    let runner_home = dir.join("runner-home");
    create_dir(&runner_home);
    write_file(
        &runner_home.join(".runner"),
        "{\"token\": \"fake-runner-token-7f3a\"}\n",
    );
    write_exec(
        &bin.join("systemctl"),
        &format!(
            "#!/usr/bin/env bash\ncase \"$*\" in\n*WorkingDirectory*) echo {h} ;;\n*) echo ;;\nesac\n",
            h = runner_home.display()
        ),
    );
    let sudoers = dir.join("sudoers.d");
    create_dir(&sudoers);
    if o.sudoers {
        write_file(
            &sudoers.join("boss-ops-runner"),
            "# fake-sudoers-line-7f3a\n",
        );
    }

    // --- the cluster: the offsite key's Secret, and Talos -----------------
    let kubectl = if o.gcs_secret {
        format!(
            "#!/usr/bin/env bash\nprintf 'kubectl %s\\n' \"$*\" >>{c}\ncase \"$*\" in\n\
             *sa*json*) printf '%s' {sa} ;;\n*bucket*) printf '%s' {bk} ;;\n*) exit 1 ;;\nesac\n",
            c = calls.display(),
            sa = b64("{\"fake\": \"fake-gcs-key-7f3a\"}\n"),
            bk = b64("fake-bucket")
        )
    } else {
        format!(
            "#!/usr/bin/env bash\nprintf 'kubectl %s\\n' \"$*\" >>{c}\n\
             echo 'Error from server (NotFound): secrets \"boss-gcs-offsite\" not found' >&2\nexit 1\n",
            c = calls.display()
        )
    };
    write_exec(&bin.join("kubectl"), &kubectl);
    let secrets_yaml = format!(
        "cluster:\n    id: fake-cluster-id\n    secret: fake-talos-bundle-7f3a\ncerts:\n    etcd:\n        crt: {etcd}\n        key: fake-talos-bundle-7f3a\n    k8s:\n        crt: {k8s}\n        key: fake-talos-bundle-7f3a\n    os:\n        crt: {os}\n        key: fake-talos-bundle-7f3a\n",
        etcd = b64("fake etcd ca\n"),
        k8s = b64(K8S_CA),
        os = b64(o.derived_os_ca)
    );
    let bundle_src = dir.join("bundle.yaml");
    write_file(&bundle_src, &secrets_yaml);
    // `read` prints the raw config; `get machineconfig -o yaml` wraps the
    // same document in a resource, four spaces in under `spec:`, followed
    // by a second resource (the shape talos_check_declared_sh.rs records).
    // `gen secrets` refuses a config that does not open at its own top
    // level with `version: v1alpha1` — so a fallback that forgot to
    // dedent the spec fails here the way talosctl would.
    let read_arm = if o.talos_read_works {
        "printf 'version: v1alpha1\\nmachine:\\n    type: controlplane\\n    token: fake-machine-config-7f3a\\n'"
    } else {
        "echo 'rpc error: code = PermissionDenied' >&2; exit 1"
    };
    write_exec(
        &bin.join("talosctl"),
        &format!(
            "#!/usr/bin/env bash\nprintf 'talosctl %s\\n' \"$*\" >>{c}\n\
             case \" $* \" in\n\
             *' read '*) {read_arm} ;;\n\
             *' get machineconfig '*) printf 'node: 192.0.2.11\\nmetadata:\\n    id: v1alpha1\\nspec:\\n    version: v1alpha1\\n    machine:\\n        type: controlplane\\n        token: fake-machine-config-7f3a\\n---\\nnode: 192.0.2.11\\nmetadata:\\n    id: persistent\\nspec:\\n    version: v1alpha1\\n' ;;\n\
             *' gen secrets '*)\n\
               out=''; cfg=''\n\
               while [ $# -gt 0 ]; do case $1 in --output-file) out=$2 ;; --from-controlplane-config) cfg=$2 ;; esac; shift; done\n\
               [ \"$(head -n1 \"$cfg\")\" = 'version: v1alpha1' ] || {{ echo 'failed to load config' >&2; exit 1; }}\n\
               cat {b} > \"$out\" ;;\n\
             *) exit 1 ;;\n\
             esac\n",
            c = calls.display(),
            b = bundle_src.display()
        ),
    );

    // --- the jobs API, as the reader reaches it ---------------------------
    let patches = dir.join("patches");
    write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\nprintf 'curl %s\\n' \"$*\" >>{c}\n\
             body=''; patch=no\n\
             while [ $# -gt 0 ]; do case $1 in --data-binary) body=$2; shift ;; PATCH) patch=yes ;; esac; shift; done\n\
             if [ $patch = yes ]; then printf '%s\\n' \"$body\" >>{p}; printf 200; else printf '{{\"metadata\":{{}}}}'; fi\n",
            c = calls.display(),
            p = patches.display()
        ),
    );

    // --- the writing machine: ssh, sudo, gpg, sync, udisksctl -------------
    // ssh drops its options and the host; a request that is not `sudo …`
    // is what a forced-command key sends, and gets the forced command.
    write_exec(
        &bin.join("ssh"),
        &format!(
            "#!/usr/bin/env bash\nprintf 'ssh %s\\n' \"$*\" >>{c}\n\
             while [ $# -gt 0 ]; do case $1 in -J|-i|-o) shift 2 ;; -*) shift ;; *) break ;; esac; done\n\
             shift\n\
             if [ \"${{1:-}}\" = sudo ]; then exec \"$@\"; fi\n\
             set -f; exec sudo -n {READER} $*\n",
            c = calls.display()
        ),
    );
    write_exec(
        &bin.join("sudo"),
        &format!(
            "#!/usr/bin/env bash\n[ \"$1\" = -n ] && shift\n\
             [ \"$1\" = {READER} ] || {{ echo \"sudo: $1 is not granted\" >&2; exit 1; }}\n\
             shift\nexec bash {r} \"$@\"\n",
            r = repo_root().join(READER_SRC).display()
        ),
    );
    write_exec(
        &bin.join("sync"),
        &format!(
            "#!/usr/bin/env bash\necho sync >>{c}\n",
            c = calls.display()
        ),
    );
    write_exec(
        &bin.join("udisksctl"),
        &format!(
            "#!/usr/bin/env bash\nprintf 'udisksctl %s\\n' \"$*\" >>{c}\n\
             if [ \"$1\" = mount ]; then\n\
               [ \"${{BOSS_TEST_MOUNT_FAIL:-0}}\" = 1 ] && {{ echo 'Error mounting /dev/fakestick: busy' >&2; exit 1; }}\n\
               echo \"Mounted /dev/fakestick at $BOSS_TEST_STICK\"\n\
             fi\nexit 0\n",
            c = calls.display()
        ),
    );
    // lsblk's RM and HOTPLUG for the stick; a test sets them to 0 0 to
    // stand in for a fixed disk.
    write_exec(
        &bin.join("lsblk"),
        &format!(
            "#!/usr/bin/env bash\nprintf 'lsblk %s\\n' \"$*\" >>{c}\n\
             echo \" ${{BOSS_TEST_RM:-1}}       ${{BOSS_TEST_HOTPLUG:-0}}\"\n",
            c = calls.display()
        ),
    );
    write_gpg(&bin, &calls, false);

    write_file(
        &dir.join("sor.env"),
        "BOSS_FORGE_HOST=192.0.2.15\nBOSS_JOBS_URL=http://127.0.0.1:9\n",
    );
    Case {
        log: dir.join("log/recovery-kit.log"),
        dir,
        bin,
        kit,
        calls,
        stick,
        patches,
    }
}

/// gpg as the identity cipher, recording what it was asked to do.
/// `corrupt` appends a byte on encryption: a stick that did not keep it.
fn write_gpg(bin: &Path, calls: &Path, corrupt: bool) {
    let enc = if corrupt {
        "{ cat; printf x; } > \"$out\""
    } else {
        "cat > \"$out\""
    };
    write_exec(
        &bin.join("gpg"),
        &format!(
            "#!/usr/bin/env bash\nprintf 'gpg %s\\n' \"$*\" >>{c}\n\
             mode=''; out=''; file=''\n\
             while [ $# -gt 0 ]; do case $1 in\n\
             --symmetric) mode=enc ;; --decrypt) mode=dec ;;\n\
             --output) out=$2; shift ;;\n\
             --cipher-algo|--s2k-mode|--s2k-digest-algo|--s2k-count) shift ;;\n\
             --*) ;; *) file=$1 ;; esac; shift; done\n\
             case $mode in enc) {enc} ;; dec) cat \"$file\" ;; *) exit 2 ;; esac\n",
            c = calls.display()
        ),
    );
}

fn env_for(c: &Case) -> Vec<(String, String)> {
    let path = format!(
        "{}:{}",
        c.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    vec![
        ("PATH".into(), path.clone()),
        ("BOSS_RECOVERY_KIT_DIR".into(), c.kit.display().to_string()),
        ("BOSS_RECOVERY_KIT_UID".into(), uid()),
        ("BOSS_RECOVERY_KIT_LOG".into(), c.log.display().to_string()),
        (
            "BOSS_OPS_DIR".into(),
            c.dir.join("ops").display().to_string(),
        ),
        (
            "BOSS_GITHUB_TOKEN_FILE".into(),
            c.dir.join("publish/github.token").display().to_string(),
        ),
        (
            "BOSS_RECOVERY_KIT_SUDOERS_DIR".into(),
            c.dir.join("sudoers.d").display().to_string(),
        ),
        (
            "BOSS_SOR_ENV".into(),
            c.dir.join("sor.env").display().to_string(),
        ),
        ("BOSS_FORGE_BACKUP_MIN_MB".into(), "0".into()),
        ("HOST_ID".into(), "forge".into()),
        ("BOSS_TEST_STICK".into(), c.stick.display().to_string()),
        // The reader sets its own PATH (re-review N1); this is its seam,
        // so the stubs stand in for the host's tools.
        ("BOSS_RECOVERY_KIT_PATH".into(), path),
    ]
}

fn command(c: &Case, script: &str, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(script))
        .args(args)
        .current_dir(repo_root())
        .stdin(Stdio::null());
    cmd.env_remove("OPS_REQUEST_ID")
        .env_remove("BOSS_RUN_SUMMARY_FILE")
        .env_remove("BOSS_JOBS_URL")
        .env_remove("BOSS_KIT_SSH_KEY")
        .env_remove("BOSS_KIT_SSH_USER")
        .env_remove("BOSS_KIT_JUMP");
    for (k, v) in env_for(c) {
        cmd.env(k, v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().unwrap_or_else(|e| panic!("run {script}: {e}"))
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn assert_no_sentinel(what: &str, s: &str) {
    for sentinel in SENTINELS {
        assert!(
            !s.contains(sentinel),
            "{what} carries the credential sentinel {sentinel} — a secret value reached the record:\n{s}"
        );
    }
}

/// The value after `<key> ` on the manifest line that starts with it.
fn field(manifest: &str, key: &str) -> String {
    manifest
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key} ")))
        .unwrap_or_else(|| panic!("the manifest has no `{key}` line:\n{manifest}"))
        .trim()
        .to_string()
}

fn assemble_with(c: &Case, extra: &[(&str, &str)]) -> (i32, String, String) {
    let out = command(c, SCRIPT, &["--assemble"], extra);
    let (so, se) = text(&out);
    assert_no_sentinel("the verb's output", &format!("{so}{se}"));
    (out.status.code().unwrap_or(-1), so, se)
}

fn assemble(c: &Case) -> (i32, String, String) {
    assemble_with(c, &[])
}

fn reader(c: &Case, args: &[&str]) -> Output {
    command(c, READER_SRC, args, &[])
}

fn items(so: &str) -> Vec<&str> {
    so.lines().filter(|l| l.starts_with("ITEM ")).collect()
}

// ---------------------------------------------------------------------------
// The cut
// ---------------------------------------------------------------------------

#[test]
fn a_whole_kit_is_cut_into_the_tmpfs_and_no_secret_reaches_the_record() {
    let c = case("whole", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(
        code, 0,
        "a kit with every item must exit 0\nstdout:\n{so}\nstderr:\n{se}"
    );
    for item in [
        "secrets/github.token",
        "secrets/talosconfig",
        "secrets/kubeconfig",
        "runner/.runner",
        "sudoers/boss-ops-runner",
        "gcs/sa.json",
        "gcs/bucket",
        "talos/controlplane.yaml",
        "talos/secrets.yaml",
    ] {
        assert!(
            items(&so).iter().any(|l| l.ends_with(&format!(" {item}"))),
            "the manifest names no ITEM {item}:\n{so}"
        );
    }
    assert!(
        items(&so)
            .iter()
            .any(|l| l.contains(" forge/forge-") && l.ends_with(".tar.gz")),
        "the manifest names no forge dump:\n{so}"
    );
    assert!(!so.contains("\nMISSING "), "nothing is missing here:\n{so}");
    assert_eq!(field(&so, "completeness"), "complete");

    // The version is sha256 over the ITEM lines, as printed.
    let joined: String = items(&so).iter().map(|l| format!("{l}\n")).collect();
    let full = sha256_of(joined.as_bytes());
    assert_eq!(field(&so, "kit_version_sha256"), full);
    let version = field(&so, "kit_version");
    assert_eq!(version, full[..12]);

    assert!(
        so.contains("talos_bundle_verified os CA = talosconfig ca; k8s CA = kubeconfig certificate-authority-data"),
        "the bundle's verification is not recorded:\n{so}"
    );

    // What the reader will stream is exactly what the manifest vouches for.
    let tar = std::fs::read(c.kit.join("kit.tar")).expect("kit.tar in the tmpfs");
    assert_eq!(field(&so, "tar_sha256"), sha256_of(&tar));
    assert_eq!(field(&so, "tar_bytes"), tar.len().to_string());
    let listing = Command::new("tar")
        .arg("-tf")
        .arg(c.kit.join("kit.tar"))
        .output()
        .expect("list kit.tar");
    let listing = String::from_utf8_lossy(&listing.stdout);
    for p in [
        "README.txt",
        "MANIFEST.txt",
        "talos/secrets.yaml",
        "secrets/github.token",
    ] {
        assert!(
            listing.contains(&format!("boss-recovery-kit-{version}/{p}")),
            "kit.tar does not hold {p} under its versioned directory:\n{listing}"
        );
    }
    assert!(
        !c.kit.join(format!("boss-recovery-kit-{version}")).exists(),
        "the unpacked kit was left beside its tar"
    );
    let state = read(&c.kit.join("state"));
    assert!(
        state.contains(&format!("version={version}\n"))
            && state.contains("complete=yes\n")
            && state.contains("missing=\n"),
        "the state the reader serves: {state}"
    );

    // An owner-only tmpfs, asked for noswap; the kernel took it.
    let calls = read(&c.calls);
    assert!(
        calls.lines().any(|l| l.starts_with("mount -t tmpfs")
            && l.contains("mode=0700")
            && l.contains("noswap")),
        "the kit directory was not mounted as an owner-only, unswappable tmpfs:\n{calls}"
    );
    assert_eq!(field(&so, "tmpfs_swap"), "noswap");
    // Every cluster call under a bound.
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("talosctl") && l.contains(" read /system/state/config.yaml")),
        "the machine config was not read from a control-plane node:\n{calls}"
    );
    let verb = read(&repo_root().join(SCRIPT));
    for tool in ["talosctl", "kubectl"] {
        let bare: Vec<&str> = verb
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with('#')
                    && t.contains(&format!("{tool} --"))
                    && !t.contains("timeout \"$CALL_TIMEOUT\"")
            })
            .collect();
        assert!(bare.is_empty(), "{tool} called without a timeout: {bare:?}");
    }
    // The forge dump is bounded too (re-review N6).
    let dump: Vec<&str> = verb
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains("env -u BOSS_RUN_SUMMARY_FILE"))
        .collect();
    assert!(
        !dump.is_empty()
            && dump
                .iter()
                .all(|l| l.contains("timeout \"$FORGE_TIMEOUT\"")),
        "forge-backup.sh runs without a timeout: {dump:?}"
    );

    // The one line: the tree's writer, with the version; the stick is the
    // only blank. And the writer's hash at cut time, to check a checkout by.
    assert!(
        so.lines()
            .any(|l| l == format!("bash {WRITER} {version} STICK_MOUNT_PATH")),
        "the rendered line is not the writer invocation:\n{so}"
    );
    let writer_sha = sha256_of(&std::fs::read(repo_root().join(WRITER)).expect("the writer"));
    assert_eq!(
        field(&so, "writer"),
        format!("{WRITER} sha256 {writer_sha}")
    );
    assert!(
        !so.contains("sudo -n"),
        "the packet carries a command body again:\n{so}"
    );
}

#[test]
fn a_missing_item_is_named_and_the_rest_is_still_cut_and_marked_incomplete() {
    let c = case(
        "missing",
        Opts {
            github_token: false,
            sudoers: false,
            gcs_secret: false,
            ..Opts::default()
        },
    );
    let (code, so, se) = assemble(&c);
    assert_eq!(
        code, 3,
        "an incomplete kit exits 3 — cut, and not whole\n{so}\n{se}"
    );
    for item in ["secrets/github.token", "sudoers/", "gcs/sa.json"] {
        assert!(
            so.lines()
                .any(|l| l.starts_with(&format!("MISSING {item}"))),
            "the manifest does not name {item} as MISSING:\n{so}"
        );
    }
    assert!(
        so.starts_with("INCOMPLETE"),
        "an incomplete kit must say so first:\n{so}"
    );
    assert_eq!(
        field(&so, "completeness"),
        "INCOMPLETE missing=secrets/github.token,sudoers/,gcs/sa.json,gcs/bucket"
    );
    let state = read(&c.kit.join("state"));
    assert!(
        state.contains("complete=no\n")
            && state.contains("missing=secrets/github.token,sudoers/,gcs/sa.json,gcs/bucket\n"),
        "the state carries no completeness: {state}"
    );
    assert!(
        items(&so)
            .iter()
            .any(|l| l.ends_with(" talos/secrets.yaml"))
    );
    assert!(c.kit.join("kit.tar").exists());
}

#[test]
fn a_node_that_refuses_the_raw_read_still_yields_the_bundle_through_its_resource() {
    let c = case(
        "get-mc",
        Opts {
            talos_read_works: false,
            ..Opts::default()
        },
    );
    let (code, so, se) = assemble(&c);
    assert_eq!(
        code, 0,
        "the resource read is a whole second route\n{so}\n{se}"
    );
    assert!(
        items(&so)
            .iter()
            .any(|l| l.ends_with(" talos/secrets.yaml")),
        "{so}"
    );
    assert!(
        so.contains("talos_bundle_verified os CA = talosconfig ca; k8s CA"),
        "{so}"
    );
    assert!(
        read(&c.calls)
            .lines()
            .any(|l| l.starts_with("talosctl") && l.contains(" get machineconfig v1alpha1 -o yaml")),
    );
}

/// Review finding 4: a bundle that is not this cluster's loses talos/ and
/// nothing else — a refusal that leaves no stick is the costlier failure.
#[test]
fn a_bundle_whose_ca_is_not_this_clusters_drops_talos_and_keeps_the_kit() {
    let c = case(
        "wrong-ca",
        Opts {
            derived_os_ca: "fake some other cluster ca\n",
            ..Opts::default()
        },
    );
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 3, "{so}\n{se}");
    let refusal = so
        .lines()
        .find(|l| l.starts_with("MISSING talos/ — REFUSED"))
        .unwrap_or_else(|| panic!("talos/ is not named MISSING REFUSED:\n{so}"));
    assert!(
        refusal.contains("DIFFERS"),
        "the refusal should say which CA differed: {refusal}"
    );
    assert!(
        !items(&so).iter().any(|l| l.contains(" talos/")),
        "a refused bundle rode along:\n{so}"
    );
    for kept in ["secrets/github.token", "gcs/sa.json", "secrets/kubeconfig"] {
        assert!(
            items(&so).iter().any(|l| l.ends_with(&format!(" {kept}"))),
            "{kept} was dropped with the bundle:\n{so}"
        );
    }
    assert!(
        c.kit.join("kit.tar").exists(),
        "the rest of the kit must stay writable"
    );
    assert!(field(&so, "completeness").contains("talos/"));
}

#[test]
fn a_kit_directory_that_is_not_a_tmpfs_gets_no_credential() {
    let c = case(
        "not-tmpfs",
        Opts {
            fstype: "ext4",
            ..Opts::default()
        },
    );
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 1, "{so}\n{se}");
    assert!(
        format!("{so}{se}").contains("tmpfs"),
        "the refusal should name what it wanted"
    );
    let calls = read(&c.calls);
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("talosctl") || l.starts_with("kubectl")),
        "a credential was fetched before the tmpfs was proven:\n{calls}"
    );
    let found = Command::new("grep")
        .args(["-rl", "fake-"])
        .arg(&c.kit)
        .output()
        .expect("grep the kit dir");
    assert!(
        found.stdout.is_empty(),
        "a credential landed on a directory that is not a tmpfs: {}",
        String::from_utf8_lossy(&found.stdout)
    );
}

/// Review finding 5: a cut that stops after the mount leaves nothing
/// resident — the EXIT trap discards what it had taken.
#[test]
fn a_cut_that_fails_after_the_mount_leaves_nothing_resident() {
    let c = case(
        "fails",
        Opts {
            github_token: false,
            sudoers: false,
            gcs_secret: false,
            ..Opts::default()
        },
    );
    // Nothing takeable: the dump refuses, the credentials are gone.
    write_exec(
        &c.bin.join("docker"),
        "#!/usr/bin/env bash\n[ \"$1\" = inspect ] && echo false\nexit 0\n",
    );
    for f in ["ops/talosconfig", "ops/kubeconfig", "runner-home/.runner"] {
        std::fs::remove_file(c.dir.join(f)).expect("remove a source");
    }
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 1, "{so}\n{se}");
    assert!(se.contains("no item could be taken"), "{se}");
    let calls = read(&c.calls);
    assert!(
        calls.lines().any(|l| l.starts_with("umount")),
        "a failed cut left its tmpfs mounted:\n{calls}"
    );
    let left: Vec<_> = std::fs::read_dir(&c.kit)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(
        left.is_empty(),
        "a failed cut left {left:?} in the kit directory"
    );
}

/// A PATH dir whose `sha256sum` and `sed` lie on cue, ahead of the case's
/// stubs, so the cut's own read-back — kit.tar re-hashed, MANIFEST.txt
/// against it, the state file's version — has something to catch. The
/// first hash of kit.tar (the one the manifest records) is always true;
/// `BOSS_TEST_REHASH` makes the SECOND one `differ` or come back `empty`,
/// and `BOSS_TEST_STATE=wrong` makes the state file read back as another
/// kit once it exists.
fn lying_path(c: &Case) -> String {
    let dir = c.dir.join("liar");
    create_dir(&dir);
    let count = c.dir.join("rehash-count");
    write_exec(
        &dir.join("sha256sum"),
        &format!(
            "#!/usr/bin/env bash\n\
             real() {{ PATH=\"${{PATH#*:}}\" command sha256sum \"$@\"; }}\n\
             case \"${{!#}}\" in */kit.tar)\n\
               n=$(( $(cat {count} 2>/dev/null || echo 0) + 1 )); echo \"$n\" > {count}\n\
               if [ \"$n\" -ge 2 ]; then case \"${{BOSS_TEST_REHASH:-}}\" in\n\
                 differ) echo \"0000000000000000000000000000000000000000000000000000000000000000  ${{!#}}\"; exit 0 ;;\n\
                 empty) exit 1 ;;\n\
               esac; fi ;;\n\
             esac\n\
             real \"$@\"\n",
            count = count.display()
        ),
    );
    write_exec(
        &dir.join("sed"),
        "#!/usr/bin/env bash\n\
         if [ \"${BOSS_TEST_STATE:-}\" = wrong ] && [ \"${2:-}\" = 's/^version=//p' ] && [ -e \"${3:-}\" ]; then\n\
           echo 0000deadbeef; exit 0\n\
         fi\n\
         PATH=\"${PATH#*:}\" exec sed \"$@\"\n",
    );
    format!(
        "{}:{}:{}",
        dir.display(),
        c.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// THE CUT'S READ-BACK IS PINNED (the delta review of the effect-proof
/// car b3ffc175, finding 1; backlog 1058e686): deleting all three checks
/// after the kit is written left every other test here green. Each check
/// is now handed the defect it exists for — a kit.tar that cannot be
/// re-hashed, a MANIFEST.txt that does not vouch for the tar on disk, a
/// state file naming another kit — and each must refuse, print no
/// COMPLETE, and leave the tmpfs empty and unmounted through the EXIT
/// trap.
#[test]
fn a_cut_whose_read_back_disagrees_keeps_nothing() {
    for (name, env, says) in [
        (
            "rehash-empty",
            ("BOSS_TEST_REHASH", "empty"),
            "REFUSED — kit.tar could not be re-hashed after the cut",
        ),
        (
            "rehash-differs",
            ("BOSS_TEST_REHASH", "differ"),
            "REFUSED — MANIFEST.txt does not read back as vouching for kit.tar",
        ),
        (
            "state-wrong",
            ("BOSS_TEST_STATE", "wrong"),
            "/state does not read back as kit ",
        ),
    ] {
        let c = case(&format!("readback-{name}"), Opts::default());
        let path = lying_path(&c);
        let (code, so, se) = assemble_with(&c, &[("PATH", path.as_str()), env]);
        assert_eq!(code, 1, "{name}: {so}\n{se}");
        assert!(se.contains(says), "{name}: expected {says:?}:\n{se}");
        assert!(!so.contains("COMPLETE —"), "{name}: no verdict line: {so}");
        let left: Vec<_> = std::fs::read_dir(&c.kit)
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{name}: the refused cut left {left:?}");
        assert!(
            read(&c.calls).lines().any(|l| l.starts_with("umount")),
            "{name}: the refused cut left its tmpfs mounted"
        );
    }
}

/// Review finding 7: a kernel without tmpfs noswap still cuts, and says
/// what swap means for it.
#[test]
fn a_kernel_without_noswap_names_the_swap_the_kit_can_reach() {
    let c = case(
        "swap",
        Opts {
            noswap_ok: false,
            swap_on: true,
            ..Opts::default()
        },
    );
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let note = field(&so, "tmpfs_swap");
    assert!(
        note.starts_with("WARNING") && note.contains("/dev/fakeswap"),
        "the swap exposure is not named: {note}"
    );
    assert!(se.contains("WARNING"), "and said loudly: {se}");
}

#[test]
fn discard_drops_the_kit_and_unmounts() {
    let c = case("discard", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let out = command(&c, SCRIPT, &["--discard"], &[]);
    assert!(out.status.success(), "{}", text(&out).1);
    assert!(!c.kit.join("kit.tar").exists());
    assert!(read(&c.calls).lines().any(|l| l.starts_with("umount")));
    // The line the verb's declared effect names follows the read-back
    // (backlog fdbb447e part 1).
    let (so, _) = text(&out);
    assert!(
        so.contains("discarded;") && so.contains("read back empty and unmounted"),
        "{so}"
    );
    // Nothing cut is an answer, not a failure.
    let again = command(&c, SCRIPT, &["--discard"], &[]);
    assert!(again.status.success());
    assert!(text(&again).0.contains("no kit is cut"));
}

/// A FAILED UMOUNT IS SAID, NOT READ AS "UNMOUNTED" (backlog fdbb447e
/// part 1). discard() says when its umount fails and carries on; the
/// line after it used to read "unmounted" regardless. The contents are
/// what must be gone, so the run still succeeds — read back empty — and
/// says where the tmpfs actually stands.
#[test]
fn a_discard_whose_umount_fails_reads_back_empty_and_says_still_mounted() {
    let c = case("discard-umount-fails", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let out = command(
        &c,
        SCRIPT,
        &["--discard"],
        &[("BOSS_TEST_UMOUNT_FAIL", "1")],
    );
    let (so, se) = text(&out);
    assert!(out.status.success(), "{so}\n{se}");
    assert!(!c.kit.join("kit.tar").exists());
    assert!(
        so.contains("read back empty and still a mounted tmpfs"),
        "{so}\n{se}"
    );
    assert!(!so.contains("empty and unmounted"), "{so}");
}

/// A DISCARD THAT LEFT THE KIT IS A FAILURE, NOT "DISCARDED" (backlog
/// fdbb447e part 1). An `rm` that removes nothing stands in for any wipe
/// that did not take: the read-back finds the kit still there, and the
/// verb fails naming what it found.
#[test]
fn a_discard_that_left_the_kit_resident_fails_by_name() {
    let c = case("discard-left-resident", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let rm_noop = c.dir.join("rm-noop");
    create_dir(&rm_noop);
    write_exec(&rm_noop.join("rm"), "#!/usr/bin/env bash\nexit 0\n");
    let path = format!(
        "{}:{}:{}",
        rm_noop.display(),
        c.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = command(&c, SCRIPT, &["--discard"], &[("PATH", path.as_str())]);
    let (so, se) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{so}\n{se}");
    assert!(
        se.contains("FAILED — kit") && se.contains("is NOT discarded"),
        "{so}\n{se}"
    );
    assert!(!so.contains("discarded;"), "no success line: {so}");
}

/// FINDMNT'S SILENCE IS NOT "NO KIT" (the delta review of b3ffc175,
/// finding 2; backlog 1058e686). is_tmpfs reads findmnt's output, and
/// findmnt that ERRORS prints nothing, exactly as for a directory that
/// is not a mount point — so a discard asked while findmnt could not
/// read the mount table said "no kit is cut", exited 0 and was judged
/// proven over a kit still resident. The directory is now read back
/// before that line may print.
#[test]
fn a_discard_that_cannot_see_the_mount_does_not_say_no_kit() {
    let c = case("discard-findmnt-errors", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let broken = c.dir.join("findmnt-broken");
    create_dir(&broken);
    write_exec(
        &broken.join("findmnt"),
        "#!/usr/bin/env bash\necho 'findmnt: cannot open /proc/self/mountinfo' >&2\nexit 1\n",
    );
    let path = format!(
        "{}:{}:{}",
        broken.display(),
        c.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = command(&c, SCRIPT, &["--discard"], &[("PATH", path.as_str())]);
    let (so, se) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{so}\n{se}");
    assert!(
        se.contains("FAILED — findmnt does not show") && se.contains("a kit may be resident"),
        "{so}\n{se}"
    );
    assert!(!so.contains("no kit is cut"), "{so}");
    assert!(c.kit.join("kit.tar").exists(), "nothing is discarded blind");
}

// ---------------------------------------------------------------------------
// The reader
// ---------------------------------------------------------------------------

#[test]
fn the_reader_streams_the_tar_the_manifest_vouches_for_and_logs_who_pulled_it() {
    let c = case("stream", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");

    let out = reader(&c, &["--stream", &version]);
    assert!(out.status.success(), "{}", text(&out).1);
    let tar = std::fs::read(c.kit.join("kit.tar")).expect("kit.tar");
    assert!(
        out.stdout == tar,
        "the stream is not the kit's tar byte for byte"
    );
    assert!(
        read(&c.log).contains(&format!("streamed kit {version}")),
        "the stream left no local record: {}",
        read(&c.log)
    );

    let m = reader(&c, &["--manifest", &version]);
    assert!(m.status.success());
    let manifest = String::from_utf8_lossy(&m.stdout).into_owned();
    assert_eq!(field(&manifest, "tar_sha256"), sha256_of(&tar));
    assert_no_sentinel("the manifest", &manifest);

    let r = reader(&c, &["--readme", &version]);
    assert!(r.status.success());
    let readme = String::from_utf8_lossy(&r.stdout).into_owned();
    assert!(
        readme.contains("RESTORE ORDER"),
        "no restore order:\n{readme}"
    );
    assert!(readme.contains(&version));
    assert!(
        readme.contains("192.0.2.15"),
        "the README does not name the forge it was cut on"
    );
    assert!(
        readme.contains(READER),
        "the README does not name the reader's door"
    );
    assert!(
        !readme.contains("{{"),
        "a placeholder was left unrendered:\n{readme}"
    );
    assert_no_sentinel("the README", &readme);
}

#[test]
fn the_reader_streams_nothing_for_another_version_a_changed_tar_or_a_stray_word() {
    let c = case("wrong", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");

    let out = reader(&c, &["--stream", "000000000000"]);
    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "a refused stream must put NOTHING on stdout — gpg would encrypt it"
    );
    assert!(
        text(&out).1.contains(&version),
        "the refusal should name the kit that IS here"
    );

    for stray in [
        &["--assemble"][..],
        &["--stream", "*"],
        &["--stream", "../../etc/shadow"],
    ] {
        let out = reader(&c, stray);
        assert!(!out.status.success(), "the reader accepted {stray:?}");
        assert!(out.stdout.is_empty());
    }

    let mut tar = std::fs::read(c.kit.join("kit.tar")).expect("kit.tar");
    tar.push(b'x');
    std::fs::write(c.kit.join("kit.tar"), &tar).expect("tamper");
    let out = reader(&c, &["--stream", &version]);
    assert!(!out.status.success(), "a changed tar must refuse");
    assert!(out.stdout.is_empty(), "a changed tar must stream nothing");
}

#[test]
fn written_refuses_a_hash_that_is_not_the_kit_and_keeps_it() {
    let c = case("written", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");
    let out = reader(&c, &["--written", &version, &"0".repeat(64)]);
    assert!(!out.status.success());
    assert!(
        c.kit.join("kit.tar").exists(),
        "a refused write record discarded the kit"
    );
    assert!(!read(&c.log).contains("wrote kit"));
}

/// Re-review N3: junk before or after a valid version or hash is refused
/// by the SHAPE check (exit 2), not left to the equality check behind it —
/// so unanchoring either regex turns this red.
#[test]
fn a_version_or_hash_with_junk_around_it_is_refused_by_its_shape() {
    let c = case("anchors", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");
    let sha = field(&so, "tar_sha256");
    for bad in [format!("{version}x"), format!("x{version}")] {
        let out = reader(&c, &["--stream", &bad]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "version {bad:?}: {}",
            text(&out).1
        );
        assert!(
            text(&out).1.contains("12 hex characters"),
            "{}",
            text(&out).1
        );
        assert!(out.stdout.is_empty());
    }
    for bad in [format!("{sha}x"), format!("x{sha}")] {
        let out = reader(&c, &["--written", &version, &bad]);
        assert_eq!(out.status.code(), Some(2), "hash {bad:?}: {}", text(&out).1);
        assert!(
            text(&out).1.contains("64 hex characters"),
            "{}",
            text(&out).1
        );
    }
    assert!(
        c.kit.join("kit.tar").exists(),
        "a refused argument discarded the kit"
    );
}

// ---------------------------------------------------------------------------
// The writer, end to end
// ---------------------------------------------------------------------------

fn write(c: &Case, version: &str, extra: &[(&str, &str)]) -> Output {
    let stick = c.stick.display().to_string();
    command(c, WRITER, &[version, &stick], extra)
}

#[test]
fn the_writer_streams_through_gpg_remounts_the_stick_reads_it_back_and_the_forge_discards() {
    let c = case("write", Opts::default());
    let (code, so, se) = assemble_with(&c, &[("OPS_REQUEST_ID", REQUEST)]);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");
    let tar = std::fs::read(c.kit.join("kit.tar")).expect("kit.tar");

    let out = write(&c, &version, &[]);
    let (wo, we) = text(&out);
    assert!(
        out.status.success(),
        "the write failed\nstdout:\n{wo}\nstderr:\n{we}"
    );
    assert_no_sentinel("the writer's output", &format!("{wo}{we}"));

    let base = c.stick.join(format!("boss-recovery-kit-{version}"));
    let enc =
        std::fs::read(base.with_extension("tar.gpg")).expect("the encrypted kit on the stick");
    assert!(
        enc == tar,
        "the identity cipher's output is not the kit's tar"
    );
    assert!(!base.with_extension("tar.gpg.partial").exists());
    let manifest = read(&base.with_extension("MANIFEST.txt"));
    assert_eq!(field(&manifest, "kit_version"), version);
    assert!(read(&base.with_extension("README.txt")).contains("RESTORE ORDER"));
    assert!(wo.contains("WRITTEN AND READ BACK FROM THE STICK"));

    let calls = read(&c.calls);
    let lines: Vec<&str> = calls.lines().collect();
    let at = |p: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(p))
            .unwrap_or_else(|| panic!("no `{p}` call:\n{calls}"))
    };
    // gpg with AES256 and the strongest OpenPGP key derivation.
    let enc_line = lines[at("gpg --symmetric")];
    for want in [
        "--no-symkey-cache",
        "--cipher-algo AES256",
        "--s2k-mode 3",
        "--s2k-digest-algo SHA512",
        "--s2k-count 65011712",
    ] {
        assert!(
            enc_line.contains(want),
            "gpg was not asked for {want}: {enc_line}"
        );
    }
    // The read-back reads the STICK: sync, unmount, mount, then decrypt.
    assert!(at("gpg --symmetric") < at("sync"));
    assert!(at("sync") < at("udisksctl unmount"));
    assert!(at("udisksctl unmount") < at("udisksctl mount"));
    assert!(
        at("udisksctl mount") < at("gpg --decrypt"),
        "decrypted before the re-mount:\n{calls}"
    );
    assert!(
        lines[at("gpg --decrypt")].contains("--no-symkey-cache"),
        "the read-back must ask for the passphrase again"
    );
    // The door is the installed reader, through its sudoers rule.
    assert!(
        lines
            .iter()
            .filter(|l| l.starts_with("ssh "))
            .all(|l| l.contains(&format!("sudo -n {READER} --"))),
        "the writer reached the forge some other way:\n{calls}"
    );

    // The forge: logged, recorded on the packet, and the kit dropped.
    let log = read(&c.log);
    assert!(
        log.contains(&format!("streamed kit {version}"))
            && log.contains(&format!("wrote kit {version}")),
        "{log}"
    );
    assert!(
        !c.kit.join("kit.tar").exists(),
        "a kit that read back was left in RAM"
    );
    let patches = read(&c.patches);
    let streamed = patches
        .lines()
        .find(|l| l.contains("recovery_kit_streamed"))
        .unwrap_or_else(|| panic!("no recovery_kit_streamed PATCH:\n{patches}"));
    assert!(streamed.contains(&version));
    let written: serde_json::Value = serde_json::from_str(
        patches
            .lines()
            .find(|l| l.contains("recovery_kit_written"))
            .unwrap_or_else(|| panic!("no recovery_kit_written PATCH:\n{patches}")),
    )
    .expect("json");
    let w = &written["recovery_kit_written"];
    assert_eq!(w["version"], version.as_str());
    assert_eq!(w["tar_sha256"], field(&manifest, "tar_sha256").as_str());
    assert_eq!(w["complete"], true);
    assert_eq!(w["missing"], serde_json::json!([]));
    // An attestation, and it says whose (re-review N2).
    assert!(
        w["proof"]
            .as_str()
            .unwrap_or("")
            .starts_with("attested by "),
        "the record claims more than an attestation: {w}"
    );
    assert!(read(&c.calls).contains(&format!("/api/jobs/{REQUEST}/metadata")));
}

#[test]
fn an_incomplete_kit_is_written_under_an_incomplete_name_and_recorded_so() {
    let c = case(
        "write-incomplete",
        Opts {
            github_token: false,
            ..Opts::default()
        },
    );
    let (code, so, se) = assemble_with(&c, &[("OPS_REQUEST_ID", REQUEST)]);
    assert_eq!(code, 3, "{so}\n{se}");
    let version = field(&so, "kit_version");
    let out = write(&c, &version, &[]);
    let (wo, we) = text(&out);
    assert!(out.status.success(), "{wo}\n{we}");
    let base = format!("boss-recovery-kit-{version}-INCOMPLETE");
    for ext in ["tar.gpg", "MANIFEST.txt", "README.txt"] {
        assert!(
            c.stick.join(format!("{base}.{ext}")).exists(),
            "no {base}.{ext} on the stick"
        );
    }
    assert!(
        !c.stick
            .join(format!("boss-recovery-kit-{version}.tar.gpg"))
            .exists(),
        "an incomplete kit was written under a complete kit's name"
    );
    assert!(
        wo.contains("INCOMPLETE KIT") && wo.contains("secrets/github.token"),
        "{wo}"
    );
    let patches = read(&c.patches);
    let written: serde_json::Value = serde_json::from_str(
        patches
            .lines()
            .find(|l| l.contains("recovery_kit_written"))
            .expect("a write record"),
    )
    .expect("json");
    assert_eq!(written["recovery_kit_written"]["complete"], false);
    assert_eq!(
        written["recovery_kit_written"]["missing"],
        serde_json::json!(["secrets/github.token"])
    );
}

#[test]
fn a_forced_command_key_reaches_the_same_reader_with_no_sudo_in_the_request() {
    let c = case("forced", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");
    let out = write(
        &c,
        &version,
        &[("BOSS_KIT_SSH_KEY", "/nonexistent/kit_key")],
    );
    assert!(out.status.success(), "{}", text(&out).1);
    let calls = read(&c.calls);
    let ssh: Vec<&str> = calls.lines().filter(|l| l.starts_with("ssh ")).collect();
    assert!(!ssh.is_empty());
    for l in &ssh {
        assert!(
            l.contains("-i /nonexistent/kit_key") && l.contains("IdentitiesOnly=yes"),
            "{l}"
        );
        assert!(
            !l.contains("sudo"),
            "a forced-command request carried the command itself: {l}"
        );
    }
    assert!(
        c.stick
            .join(format!("boss-recovery-kit-{version}.tar.gpg"))
            .exists()
    );
}

#[test]
fn a_write_that_does_not_read_back_is_never_named_a_kit_and_the_kit_stays() {
    let c = case("corrupt", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");
    write_gpg(&c.bin, &c.calls, true);

    let out = write(&c, &version, &[]);
    assert!(
        !out.status.success(),
        "a write whose read-back differs must fail"
    );
    let base = format!("boss-recovery-kit-{version}");
    assert!(
        !c.stick.join(format!("{base}.tar.gpg")).exists(),
        "an unverified write was named the kit"
    );
    assert!(
        c.stick.join(format!("{base}.tar.gpg.partial")).exists(),
        "the failed write should stay visible as .partial"
    );
    assert!(text(&out).1.contains("NOT a kit"));
    assert!(
        !read(&c.log).contains("wrote kit"),
        "the forge recorded a write that did not verify"
    );
    assert!(
        c.kit.join("kit.tar").exists(),
        "a failed write discarded the kit — there is nothing to retry from"
    );
}

/// Re-review N4: a fixed disk is not a stick. The writer unmounts what it
/// is given, so it refuses before fetching or writing anything.
#[test]
fn the_writer_refuses_a_target_that_is_not_removable_media() {
    let c = case("fixed", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");
    let out = write(
        &c,
        &version,
        &[("BOSS_TEST_RM", "0"), ("BOSS_TEST_HOTPLUG", "0")],
    );
    let (_, we) = text(&out);
    assert!(!out.status.success(), "a fixed disk was written to");
    assert!(
        we.contains("REFUSED")
            && we.contains("not removable media")
            && we.contains(&c.stick.display().to_string()),
        "the refusal must be loud and name the path: {we}"
    );
    let calls = read(&c.calls);
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("ssh ") || l.starts_with("udisksctl")),
        "the writer reached the forge or unmounted before refusing:\n{calls}"
    );
    let on_stick: Vec<_> = std::fs::read_dir(&c.stick)
        .expect("the stick dir")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(
        on_stick.is_empty(),
        "a refused target was written to: {on_stick:?}"
    );

    // HOTPLUG alone is removable enough (a USB disk that reports RM=0).
    let out = write(
        &c,
        &version,
        &[("BOSS_TEST_RM", "0"), ("BOSS_TEST_HOTPLUG", "1")],
    );
    assert!(out.status.success(), "{}", text(&out).1);
}

/// Re-review N5: a re-mount that fails says what it left behind.
#[test]
fn a_failed_remount_says_the_stick_is_left_unmounted_and_nothing_is_named_a_kit() {
    let c = case("remount-fails", Opts::default());
    let (code, so, se) = assemble(&c);
    assert_eq!(code, 0, "{so}\n{se}");
    let version = field(&so, "kit_version");
    let out = write(&c, &version, &[("BOSS_TEST_MOUNT_FAIL", "1")]);
    let (_, we) = text(&out);
    assert!(!out.status.success());
    assert!(
        we.contains("UNMOUNTED") && we.contains("NOT a kit"),
        "the failure does not say the stick's state: {we}"
    );
    let base = format!("boss-recovery-kit-{version}");
    assert!(!c.stick.join(format!("{base}.tar.gpg")).exists());
    assert!(
        !read(&c.calls).contains("gpg --decrypt"),
        "it read back without re-mounting"
    );
    assert!(
        c.kit.join("kit.tar").exists(),
        "the kit was dropped though nothing verified"
    );
    // And the path without udisksctl tells the operator the re-insert is
    // the whole point.
    let writer = read(&repo_root().join(WRITER));
    assert!(writer.contains("PULL THE STICK OUT and PLUG IT BACK IN"));
    assert!(writer.contains("reads this computer's memory, not the stick"));
}

// ---------------------------------------------------------------------------
// The door: the installed reader and its one sudoers rule
// ---------------------------------------------------------------------------

fn install(dir: &Path, visudo_ok: bool, user: &str) -> Output {
    let bin = dir.join("bin");
    create_dir(&bin);
    create_dir(&dir.join("sudoers.d"));
    write_exec(
        &bin.join("visudo"),
        &format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >>{d}/visudo-calls\ncp \"$2\" {d}/checked\nexit {rc}\n",
            d = dir.display(),
            rc = if visudo_ok { 0 } else { 1 }
        ),
    );
    Command::new("bash")
        .arg(repo_root().join(INSTALLER))
        .env("INSTALL_KIT_LIBEXEC", dir.join("libexec"))
        .env("INSTALL_KIT_SUDOERS_DIR", dir.join("sudoers.d"))
        .env("INSTALL_VISUDO", bin.join("visudo"))
        .env("INSTALL_KIT_USER", user)
        .env("INSTALL_KIT_OWNER", "")
        .output()
        .expect("run the installer")
}

#[test]
fn the_installer_places_a_copy_outside_the_checkout_and_one_checked_sudoers_rule() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("recovery-kit-install");
    let out = install(&dir, true, "kitwriter");
    let (so, se) = text(&out);
    assert!(out.status.success(), "{so}\n{se}");

    let copy = dir.join("libexec/recovery-kit-read");
    assert_eq!(
        std::fs::read(&copy).expect("the installed reader"),
        std::fs::read(repo_root().join(READER_SRC)).expect("the tree's reader"),
        "the installed reader is not the tree's"
    );
    assert_eq!(
        std::fs::metadata(&copy).expect("stat").permissions().mode() & 0o777,
        0o755
    );

    let rule = dir.join("sudoers.d/boss-recovery-kit");
    let body = read(&rule);
    let lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(
        lines,
        vec![
            format!("Defaults!{READER} env_reset").as_str(),
            format!(
                "Defaults!{READER} secure_path=\"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\""
            )
            .as_str(),
            format!("kitwriter ALL=(root) NOPASSWD: {READER}").as_str(),
        ],
        "the sudoers rule grants something other than the reader:\n{body}"
    );
    assert_eq!(
        std::fs::metadata(&rule).expect("stat").permissions().mode() & 0o777,
        0o440
    );
    assert!(
        read(&dir.join("visudo-calls")).starts_with("-cf "),
        "the rule was not checked with visudo -cf"
    );
    assert_eq!(
        read(&dir.join("checked")),
        body,
        "visudo checked something other than what was placed"
    );
    assert!(
        so.contains(&format!(
            "command=\"set -f; exec sudo -n {READER} $SSH_ORIGINAL_COMMAND\",restrict"
        )),
        "the forced-command line is not rendered for its placer:\n{so}"
    );
}

#[test]
fn a_rule_visudo_refuses_is_never_placed_and_root_is_never_the_grantee() {
    let dir = scratch_dir("recovery-kit-install-refused");
    let out = install(&dir, false, "kitwriter");
    assert!(!out.status.success());
    assert!(
        !dir.join("sudoers.d/boss-recovery-kit").exists(),
        "a rule visudo refused was placed"
    );
    let leftovers: Vec<_> = std::fs::read_dir(dir.join("sudoers.d"))
        .expect("the dir")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "a staged rule was left behind: {leftovers:?}"
    );

    let dir = scratch_dir("recovery-kit-install-root");
    let out = install(&dir, true, "root");
    assert!(
        !out.status.success(),
        "a rule granting root to root was installed"
    );
    for bad in ["ALL", "david ALL", "a b"] {
        let dir = scratch_dir("recovery-kit-install-bad-user");
        assert!(
            !install(&dir, true, bad).status.success(),
            "user {bad:?} was accepted"
        );
    }
}

/// Review finding 1, HIGH: what root runs for a workstation owes nothing
/// to a checkout its grantee can write.
#[test]
fn the_reader_sources_nothing_and_every_file_names_the_same_door() {
    let reader = read(&repo_root().join(READER_SRC));
    // Its interpreter and PATH are its own, not the caller's (re-review N1).
    assert_eq!(reader.lines().next(), Some("#!/bin/bash"));
    let path_at = reader
        .lines()
        .position(|l| l.starts_with("PATH=\"${BOSS_RECOVERY_KIT_PATH:-/usr/local/sbin:"))
        .expect("the reader sets its own PATH");
    let first_tool = reader
        .lines()
        .position(|l| {
            !l.starts_with('#')
                && (l.contains("findmnt") || l.contains("sed ") || l.contains("curl "))
        })
        .expect("the reader calls tools");
    assert!(
        path_at < first_tool,
        "the reader names a tool before it sets PATH"
    );
    let sourcing: Vec<&str> = reader
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && (t.starts_with(". ") || t.starts_with("source "))
        })
        .collect();
    assert!(sourcing.is_empty(), "the reader sources: {sourcing:?}");
    for word in ["BASH_SOURCE", "REPO", "infra/"] {
        assert!(
            !reader
                .lines()
                .any(|l| !l.trim_start().starts_with('#') && l.contains(word)),
            "the reader's code refers to its checkout ({word})"
        );
    }
    for f in [
        SCRIPT,
        WRITER,
        INSTALLER,
        "infra/forge/recovery-kit-README.txt",
    ] {
        let body = read(&repo_root().join(f));
        assert!(
            body.contains(READER) || body.contains("{{READER}}"),
            "{f} does not name the door {READER}"
        );
    }
    let writer = read(&repo_root().join(WRITER));
    assert!(
        !writer.contains("infra/forge/recovery-kit.sh"),
        "the writer reaches the checkout's script"
    );
    let install_sh = read(&repo_root().join("infra/forge/install.sh"));
    assert!(
        install_sh.contains("bash \"${HERE}/install-recovery-kit-reader.sh\""),
        "infra/forge/install.sh does not install the reader"
    );
}

#[test]
fn the_verbs_serve_the_forge_say_they_mutate_and_name_their_authority() {
    for (verb, arg, floor) in [
        ("cut-recovery-kit", "--assemble", 600),
        ("discard-recovery-kit", "--discard", 30),
    ] {
        let p = repo_root().join(format!("infra/ops/verbs/{verb}.json"));
        let v: serde_json::Value =
            serde_json::from_str(&read(&p)).unwrap_or_else(|e| panic!("{verb}: {e}"));
        assert_eq!(v["hosts"], serde_json::json!(["forge"]));
        assert_eq!(v["argv"], serde_json::json!([SCRIPT, arg]));
        assert_eq!(
            v["params"],
            serde_json::json!([]),
            "{verb} takes something from the packet"
        );
        let about = v["about"].as_str().expect("about");
        assert!(
            about.contains("MUTATING") && about.contains("David"),
            "{verb}"
        );
        assert!(
            v["timeout"].as_u64().unwrap_or(0) >= floor,
            "{verb}'s timeout"
        );
    }
}
