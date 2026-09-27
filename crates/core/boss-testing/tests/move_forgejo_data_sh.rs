//! `infra/forge/move-forgejo-data.sh` is RUN, not read — against a
//! scratch `/opt/forgejo/data`, a scratch fstab, compose file, mount
//! table and block-device listing, and stubbed `findmnt`, `lsblk`,
//! `df`, `docker`, `curl`, `rsync`, `mount`, `umount`, `chattr`,
//! `lsattr` and `systemctl` that record every call. So every verdict
//! below is one the script actually reached, the fstab edits and the
//! renames are ones it actually made, and "nothing changed" is read off
//! the files and the call log rather than trusted.
//!
//! WHY THE VERB EXISTS (backlog 016aeea3, car 2 of b2d5b546,
//! 2026-09-27). The forge's package registry under /opt/forgejo/data
//! was 92G of its root disk and drove the urgent disk alarm c4da71b0;
//! a second drive went in, commission-a-disk mounts it by UUID as
//! boss-data, and this verb moves the data there — keeping the path, so
//! the compose file and the five scripts that read /opt/forgejo/data do
//! not change — behind a rendered plan David's passkey signs, with a
//! named rollback that is its own approval verb.
//!
//! WHAT IS FAKED, stated rather than left to be discovered. There is no
//! block device, no docker and no rsync in the gate image, and the gate
//! runs as uid 65534, so: `rsync`'s copy is `cp -a` and its verify pass
//! answers what the case says; `mount` and `umount` edit the scratch
//! mount table the `findmnt` stub reads; `chattr` records and does
//! nothing. What is REAL: the script, its preconditions, its plan and
//! hash, the census of both sides, every fstab edit (trailing newline,
//! the appended line, the removed line), every rename and mkdir, the
//! converge hold file, and the ORDER of every mutating command, which
//! the call log records and a pin below also reads off the file.
//!
//! The adversarial review of commission-a-disk (the pattern this copies,
//! 2026-09-27) named five holes; each has a case here: an fstab line
//! with nofail and a device timeout, appended only through a temporary
//! file findmnt verifies, and mounted by path, never `mount -a`
//! (`the_fstab_is_edited_only_through_a_verified_file`); checks that fail
//! closed (`a_read_that_fails_is_a_refusal_never_a_value`); 78 only
//! while nothing changed, 1 after (`a_failed_copy_*`, the refusal
//! table); bounded paths and a verified target (the refusal table); and
//! the act inside the hashed plan (`the_plan_names_the_act_*`).
//!
//! The re-review of the released car (backlog 85d29e33, 2026-09-27)
//! found that a proof could stop the forge for a reason of its own and
//! nothing could start it again. Its six findings each have a case in
//! the block headed by that review below, and the restart verb it added
//! — the approved way back for a stopped Forgejo — has its own block.
//!
//! The review of THAT car (backlog ed7702c3, 2026-09-27) found a signal
//! between the rename and `STAGE=committed` could start an empty Forgejo,
//! and a restart read green with FINDINGs; its block follows the
//! restart's, and a signal is delivered after a real rename there.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/forge/move-forgejo-data.sh";
const ROOT_UUID: &str = "9f8e7d6c-aaaa-bbbb-cccc-000011112222";
const TUUID: &str = "0a1b2c3d-1111-2222-3333-444455556666";
const MAIN: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const STAMP: &str = "c85941b4e0a1f2b3c4d5e6f708192a3b4c5d6e7f";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// jq and sha256sum are the gate image's; a test of a trust-boundary
/// verb that cannot run must fail, never pass by returning early.
fn needs_tools() {
    for t in ["jq", "sha256sum", "cp", "du", "find", "stat"] {
        assert!(has(t), "move_forgejo_data_sh: no {t} on this box");
    }
}

fn put(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
    write_file(path, body);
}

fn sha256_of(path: &Path) -> String {
    let out = Command::new("sha256sum").arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

struct Case {
    root: PathBuf,
    bin: PathBuf,
    src: PathBuf,
    mnt: PathBuf,
    fstab: PathBuf,
    mounts: PathBuf,
    lsblk: PathBuf,
    hold: PathBuf,
    calls: PathBuf,
    state: PathBuf,
    env: Vec<(String, String)>,
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

impl Run {
    fn text(&self) -> String {
        format!("{}{}", self.out, self.err)
    }
    fn plan_sha(&self) -> String {
        self.err
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("no plan-sha256 line:\n{}", self.text()))
            .to_string()
    }
}

const STUB_FINDMNT: &str = r#"#!/bin/sh
echo "findmnt $*" >> "$STUB_CALLS"
[ -n "${STUB_FINDMNT_FAIL:-}" ] && { echo "findmnt: stub failure" >&2; exit 1; }
if [ "$1" = --verify ]; then
    tab="$3"
    # findmnt's own shape: a target line, then its indented messages,
    # then an unindented summary.
    case "${STUB_VERIFY_FAIL:-}" in
        now) [ "$tab" = "$STUB_FSTAB" ] && { echo "/"; echo "   [E] stub: unreachable source"; exit 1; } ;;
        new) [ "$tab" != "$STUB_FSTAB" ] && { echo "/"; echo "   [E] stub: unreachable source"; exit 1; } ;;
        drive)
            echo "$STUB_MNT"
            echo "   [E] unreachable on boot required source: UUID=$STUB_TUUID"
            if grep -q " $STUB_SRC " "$tab"; then
                echo "$STUB_SRC"
                echo "   [E] unreachable source: $STUB_MNT/forgejo-data"
            fi
            echo ""
            echo "0 parse errors, 2 errors, 0 warnings"
            exit 1 ;;
        other)
            echo "$STUB_MNT"
            echo "   [E] unreachable on boot required source: UUID=$STUB_TUUID"
            echo "/"
            echo "   [E] unreachable on boot required source: UUID=$STUB_ROOT_UUID"
            exit 1 ;;
        # A parse error drops its line, so no target carries it: only the
        # warning on stderr and the summary say it happened.
        parse)
            echo "findmnt: $tab: parse error at line 4 -- ignored" >&2
            echo "$STUB_MNT"
            echo "   [E] unreachable on boot required source: UUID=$STUB_TUUID"
            echo ""
            echo "1 parse errors, 1 errors, 0 warnings"
            exit 1 ;;
        # An error under the absent drive's own target that is not an
        # unreachable source.
        notreach)
            echo "$STUB_MNT"
            echo "   [E] unreachable on boot required source: UUID=$STUB_TUUID"
            echo "   [E] unsupported filesystem type: ext9"
            echo ""
            echo "0 parse errors, 2 errors, 0 warnings"
            exit 1 ;;
    esac
    echo "Success, no errors or warnings detected"
    exit 0
fi
cols="$2"; shift 2
case "$cols" in
  TARGET)
    if [ $# -eq 0 ]; then cat "$STUB_MOUNTS"; exit 0; fi
    p="$2"
    case "$p" in
      "$STUB_MNT"|"$STUB_MNT"/*) echo "$STUB_MNT" ;;
      *) echo "${STUB_SRC_ON:-/}" ;;
    esac ;;
  UUID)
    if [ "$1" = / ]; then echo "$STUB_ROOT_UUID"; exit 0; fi
    [ "$2" = "$STUB_MNT" ] || exit 1
    echo "${STUB_SEEN_UUID:-$STUB_TUUID}" ;;
  UUID,FSROOT)
    grep -qxF -- "$2" "$STUB_MOUNTS" || exit 1
    echo "$STUB_TUUID ${STUB_BIND_ROOT:-/forgejo-data}" ;;
  SOURCE) echo "/dev/stub[/forgejo-data]" ;;
  *) echo "stub findmnt: unexpected -o $cols" >&2; exit 99 ;;
esac
"#;

const STUB_DOCKER: &str = r#"#!/bin/sh
echo "docker $*" >> "$STUB_CALLS"
state="$STUB_STATE"
case "$1" in
  compose)
    shift
    [ "$1" = version ] && { echo "Docker Compose version v2 (stub)"; exit 0; }
    [ "$1" = -f ] && shift 2
    case "$1" in
      config) echo "${STUB_SERVICES:-forgejo}" ;;
      ps) printf '%s' "${STUB_CIDS:-$STUB_CID
}" ;;
      stop) echo false > "$state" ;;
      start)
        [ -n "${STUB_START_FAIL:-}" ] && { echo "stub: start failed" >&2; exit 1; }
        # A start of a container that is already running is a no-op.
        [ -n "${STUB_START_NOOP:-}" ] && exit 0
        echo true > "$state"
        printf '%s+\n' "$(cat "$STUB_STARTED")" > "$STUB_STARTED" ;;
      *) echo "stub docker compose: unexpected $*" >&2; exit 99 ;;
    esac ;;
  inspect)
    case "$3" in
      *StartedAt*) echo "$(cat "$state") $(cat "$STUB_STARTED") ${STUB_RESTARTS:-0}" ;;
      *RestartPolicy*) echo "${STUB_RESTART_POLICY:-always}" ;;
      *State.Running*) if [ "$4" = "$STUB_CID" ]; then cat "$state"; else echo true; fi ;;
      *Mounts*) [ "$4" = "${STUB_OTHER_ID:-}" ] && echo "$STUB_OTHER_MOUNT" ;;
    esac ;;
  ps) echo "$STUB_CID"; [ -n "${STUB_OTHER_ID:-}" ] && echo "$STUB_OTHER_ID" ;;
  exec)
    # `stat -c %d:%i /data/…` answers for the directory the container
    # SEES at /data: the bind's target while the path is mounted — or
    # the one the case names in STUB_DATA_ROOT — and the path itself
    # when not. An image with no stat fails; one whose stat prints
    # otherwise prints otherwise. Anything else is git's main.
    case " $* " in
      *" stat "*)
        [ -n "${STUB_STAT_FAIL:-}" ] && { echo "OCI runtime exec failed: stat: executable file not found in PATH" >&2; exit 126; }
        [ -n "${STUB_STAT_ODD:-}" ] && { echo "  File: /data/git/repositories/david/boss.git"; exit 0; }
        for a in "$@"; do p="$a"; done
        if grep -qxF -- "$STUB_SRC" "$STUB_MOUNTS"; then
          root="${STUB_DATA_ROOT:-$STUB_MNT/forgejo-data}"
        else
          root="$STUB_SRC"
        fi
        stat -c %d:%i "${root}${p#/data}" ;;
      *) echo "$STUB_MAIN" ;;
    esac ;;
  *) echo "stub docker: unexpected $*" >&2; exit 99 ;;
esac
exit 0
"#;

const STUB_CURL: &str = r#"#!/bin/sh
out=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    -w|-K|-H|--max-time) shift ;;
    http*) url="$1" ;;
  esac
  shift
done
echo "curl $url" >> "$STUB_CALLS"
case "$url" in
  */api/healthz)
    if [ -n "${STUB_UNHEALTHY:-}" ]; then printf '{"status":"fail"}' > "$out"; printf 503
    else printf '{"status":"pass"}' > "$out"; printf 200; fi ;;
  */v2/token*) printf '{"token":"stub-bearer"}' > "$out"; printf 200 ;;
  */manifests/*)
    # An image the registry does not hold (pushed into the moved copy
    # after the move, say) answers 404.
    if [ -n "${STUB_MISSING_TAG:-}" ] && [ "${url##*/}" = "$STUB_MISSING_TAG" ]; then printf 404
    else printf '%s' "${STUB_MANIFEST:-manifest-body}" > "$out"; printf 200; fi ;;
  *) printf 404 ;;
esac
"#;

// The copy is `cp -a <src>/. <tgt>/`; the verify pass lists what the
// case says. Both record their argv, so the flags are pinned too.
const STUB_RSYNC: &str = r#"#!/bin/sh
echo "rsync $*" >> "$STUB_CALLS"
[ -n "${STUB_RSYNC_FAIL:-}" ] && { echo "rsync: stub failure" >&2; exit 23; }
# dockerd restarting a restart:always container while the copy runs.
if [ -n "${STUB_RESTART_MIDCOPY:-}" ]; then
  echo true > "$STUB_STATE"
  printf '%s+\n' "$(cat "$STUB_STARTED")" > "$STUB_STARTED"
fi
for a in "$@"; do
  if [ "$a" = --dry-run ]; then
    [ -n "${STUB_VERIFY_DIFF:-}" ] && echo ">fc.t...... gitea/conf/app.ini"
    exit 0
  fi
done
prev=""; last=""
for a in "$@"; do prev="$last"; last="$a"; done
cp -a "${prev}." "$last"
"#;

const STUB_MOUNT: &str = r#"#!/bin/sh
echo "mount $*" >> "$STUB_CALLS"
for a in "$@"; do last="$a"; done
echo "$last" >> "$STUB_MOUNTS"
# dockerd starting a restart:always container behind the verb's back
# once the source is renamed and the bind is up.
if [ -n "${STUB_RESTART_AT_MOUNT:-}" ]; then
  echo true > "$STUB_STATE"
  printf '%s+\n' "$(cat "$STUB_STARTED")" > "$STUB_STARTED"
fi
"#;

const STUB_CHATTR: &str = r#"#!/bin/sh
echo "chattr $*" >> "$STUB_CALLS"
[ -n "${STUB_CHATTR_FAIL:-}" ] && [ "$1" = +i ] && { echo "chattr: stub failure" >&2; exit 1; }
exit 0
"#;

/// A `mv` that refuses only the move's rename of the source, and hands
/// every other call to the real one behind it on PATH.
const STUB_MV_RENAME_FAILS: &str = r#"#!/bin/sh
if [ "$1" = -T ] && [ "$3" = "$STUB_SRC" ]; then echo "mv: stub failure" >&2; exit 1; fi
PATH="${PATH#*:}" exec mv "$@"
"#;

/// A `mv` that makes the move's rename of the source FOR REAL and then
/// delivers `$STUB_SIGNAL` to the script that ran it: the window between
/// `mv -T` returning and `STAGE=committed` (review of the released
/// follow-up car, backlog ed7702c3, finding 1). Every other call goes to
/// the real `mv` behind it on PATH.
const STUB_MV_SIGNAL_AFTER_RENAME: &str = r#"#!/bin/sh
PATH="${PATH#*:}" mv "$@" || exit $?
if [ "$1" = -T ] && [ "$3" = "$STUB_SRC" ]; then kill -s "$STUB_SIGNAL" "$PPID"; fi
exit 0
"#;

const STUB_UMOUNT: &str = r#"#!/bin/sh
echo "umount $*" >> "$STUB_CALLS"
# Busy for the first STUB_UMOUNT_BUSY tries.
n=$(cat "$STUB_UMOUNT_TRIES" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$STUB_UMOUNT_TRIES"
if [ "$n" -le "${STUB_UMOUNT_BUSY:-0}" ]; then echo "umount: target is busy." >&2; exit 32; fi
for a in "$@"; do last="$a"; done
grep -vxF -- "$last" "$STUB_MOUNTS" > "$STUB_MOUNTS.new"
mv "$STUB_MOUNTS.new" "$STUB_MOUNTS"
"#;

impl Case {
    fn new(name: &str) -> Self {
        needs_tools();
        let root = scratch_dir(&format!("move-forgejo-data-{name}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let src = root.join("opt/forgejo/data");
        // A real bare repository: after the stop, host git reads main.
        let bare = src.join("git/repositories/david/boss.git");
        std::fs::create_dir_all(&bare).unwrap();
        let ok = Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&bare)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git init --bare {}", bare.display());
        put(&bare.join("refs/heads/main"), &format!("{MAIN}\n"));
        put(
            &src.join("gitea/conf/app.ini"),
            "[server]\nROOT_URL = http://forge.test/\n",
        );
        put(&src.join("gitea/packages/ab/cd/blob"), "registry bytes\n");
        put(
            &src.join("ssh/ssh_host_ed25519_key.pub"),
            "ssh-ed25519 AAAA stub\n",
        );
        let compose = root.join("opt/forgejo/docker-compose.yml");
        put(
            &compose,
            &format!(
                "services:\n  forgejo:\n    image: forgejo\n    volumes:\n      - {}:/data\n",
                src.display()
            ),
        );
        let mnt = root.join("srv/boss-data");
        std::fs::create_dir_all(&mnt).unwrap();
        let fstab = root.join("etc/fstab");
        put(
            &fstab,
            &format!(
                "# /etc/fstab\nUUID={ROOT_UUID} / ext4 errors=remount-ro 0 1\n\
                 UUID={TUUID} {} ext4 defaults,noatime,nofail,x-systemd.device-timeout=30s 0 2\n",
                mnt.display()
            ),
        );
        let mounts = root.join("mounts");
        write_file(&mounts, &format!("/\n{}\n", mnt.display()));
        let lsblk = root.join("lsblk.json");
        let c = Self {
            src: src.clone(),
            mnt: mnt.clone(),
            fstab: fstab.clone(),
            mounts: mounts.clone(),
            lsblk: lsblk.clone(),
            hold: root.join("hold"),
            calls: root.join("calls.log"),
            state: root.join("running"),
            bin: bin.clone(),
            env: Vec::new(),
            root: root.clone(),
        };
        c.set_lsblk(&[
            (
                "/dev/nvme1n1p2",
                None,
                ROOT_UUID,
                "ext4",
                vec!["/".to_string()],
            ),
            (
                "/dev/nvme0n1p1",
                Some("boss-data"),
                TUUID,
                "ext4",
                vec![mnt.display().to_string()],
            ),
        ]);
        write_file(&c.state, "true\n");
        write_file(&root.join("started"), "2026-09-27T00:00:00Z\n");
        // The process table answers for its reader, as /proc/self does;
        // a table that does not is refused (re-review finding 5).
        std::fs::create_dir_all(root.join("proc/self")).unwrap();
        std::os::unix::fs::symlink("/", root.join("proc/self/cwd")).unwrap();
        write_file(&root.join("stamp"), &format!("{STAMP}\n"));
        // base64("david:stub-token")
        write_file(
            &root.join("docker-config.json"),
            r#"{"auths": {"forge.test:3000": {"auth": "ZGF2aWQ6c3R1Yi10b2tlbg=="}}}"#,
        );
        write_exec(&bin.join("findmnt"), STUB_FINDMNT);
        write_exec(&bin.join("docker"), STUB_DOCKER);
        write_exec(&bin.join("curl"), STUB_CURL);
        write_exec(&bin.join("rsync"), STUB_RSYNC);
        write_exec(&bin.join("mount"), STUB_MOUNT);
        write_exec(&bin.join("umount"), STUB_UMOUNT);
        write_exec(
            &bin.join("lsblk"),
            "#!/bin/sh\necho \"lsblk $*\" >> \"$STUB_CALLS\"\ncat \"$STUB_LSBLK\"\n",
        );
        write_exec(
            &bin.join("df"),
            "#!/bin/sh\necho \"df $*\" >> \"$STUB_CALLS\"\necho Avail\necho \"${STUB_AVAIL:-1000000000000}\"\n",
        );
        write_exec(
            &bin.join("fuser"),
            "#!/bin/sh\necho \"fuser $*\" >> \"$STUB_CALLS\"\necho \"stub fuser: 4242 git holds $*\" >&2\n",
        );
        write_exec(&bin.join("chattr"), STUB_CHATTR);
        for t in ["lsattr", "systemctl"] {
            write_exec(
                &bin.join(t),
                &format!("#!/bin/sh\necho \"{t} $*\" >> \"$STUB_CALLS\"\nexit 0\n"),
            );
        }
        c
    }

    fn set_lsblk(&self, devs: &[(&str, Option<&str>, &str, &str, Vec<String>)]) {
        let v: Vec<serde_json::Value> = devs
            .iter()
            .map(|(n, l, u, f, m)| {
                serde_json::json!({"name": n, "label": l, "uuid": u, "fstype": f, "mountpoints": m})
            })
            .collect();
        write_file(
            &self.lsblk,
            &serde_json::json!({ "blockdevices": v }).to_string(),
        );
    }

    fn with(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.to_string(), v.to_string()));
        self
    }

    fn pre(&self) -> PathBuf {
        PathBuf::from(format!("{}.pre-move-0a1b2c3d", self.src.display()))
    }

    fn tgt(&self) -> PathBuf {
        self.mnt.join("forgejo-data")
    }

    fn fstab_line(&self) -> String {
        format!(
            "{} {} none bind,nofail,x-systemd.requires-mounts-for={},x-systemd.before=docker.service,x-systemd.device-timeout=30s 0 0",
            self.tgt().display(),
            self.src.display(),
            self.mnt.display()
        )
    }

    fn run(&self, args: &[&str]) -> Run {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT)).args(args);
        cmd.env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_CALLS", &self.calls)
            .env("STUB_MOUNTS", &self.mounts)
            .env("STUB_LSBLK", &self.lsblk)
            .env("STUB_FSTAB", &self.fstab)
            .env("STUB_STATE", &self.state)
            .env("STUB_MNT", &self.mnt)
            .env("STUB_ROOT_UUID", ROOT_UUID)
            .env("STUB_TUUID", TUUID)
            .env("STUB_CID", "cid-forgejo-0001")
            .env("STUB_MAIN", MAIN)
            .env("STUB_SRC", &self.src)
            .env("STUB_STARTED", self.root.join("started"))
            .env("STUB_UMOUNT_TRIES", self.root.join("umount-tries"))
            .env("BOSS_MOVE_PROC_DIR", self.root.join("proc"))
            .env("BOSS_MOVE_RETRY_SLEEP", "0")
            .env("BOSS_FORGEJO_DATA", &self.src)
            .env(
                "BOSS_FORGE_COMPOSE",
                self.root.join("opt/forgejo/docker-compose.yml"),
            )
            .env("BOSS_FSTAB", &self.fstab)
            .env("BOSS_CONVERGE_HOLD", &self.hold)
            .env("BOSS_FORGE_LAST_BUILT", self.root.join("stamp"))
            .env(
                "BOSS_MOVE_DOCKER_CONFIG",
                self.root.join("docker-config.json"),
            )
            .env("BOSS_FORGE_REGISTRY", "forge.test:3000/david/boss")
            .env("BOSS_FORGE_URL", "http://forge.test:3000")
            .env("BOSS_SOR_ENV", self.root.join("absent-sor.env"))
            .env("BOSS_MOVE_HEALTH_WAIT", "0");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("move-forgejo-data.sh runs");
        Run {
            code: out.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.calls)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The index of the first recorded call starting with `prefix`.
    fn call_at(&self, prefix: &str) -> usize {
        let calls = self.calls();
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` call in:\n{}", calls.join("\n")))
    }

    /// The index of the LAST recorded call starting with `prefix`.
    fn call_last(&self, prefix: &str) -> usize {
        let calls = self.calls();
        calls
            .iter()
            .rposition(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` call in:\n{}", calls.join("\n")))
    }

    /// The served marker beside the target.
    fn marker(&self) -> PathBuf {
        PathBuf::from(format!("{}.went-live", self.tgt().display()))
    }

    /// Forgejo's recorded running state, as the docker stub keeps it.
    fn running(&self) -> String {
        std::fs::read_to_string(&self.state)
            .unwrap()
            .trim()
            .to_string()
    }

    fn fstab_text(&self) -> String {
        std::fs::read_to_string(&self.fstab).unwrap()
    }

    /// Every path under the scratch /opt, /srv and /etc, with each
    /// file's bytes: what "nothing changed" is judged against.
    fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        fn walk(p: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            let meta = std::fs::symlink_metadata(p).unwrap();
            if meta.is_dir() {
                out.push((format!("{rel}/"), vec![]));
                for e in std::fs::read_dir(p).unwrap() {
                    walk(&e.unwrap().path(), root, out);
                }
            } else if meta.is_symlink() {
                out.push((
                    rel,
                    std::fs::read_link(p)
                        .unwrap()
                        .display()
                        .to_string()
                        .into_bytes(),
                ));
            } else {
                out.push((rel, std::fs::read(p).unwrap()));
            }
        }
        let mut out = Vec::new();
        for d in ["opt", "srv", "etc", "elsewhere"] {
            let p = self.root.join(d);
            if p.exists() {
                walk(&p, &self.root, &mut out);
            }
        }
        out.sort();
        out
    }

    /// Nothing on disk moved, the converge is not held, and no mutating
    /// command was so much as called.
    fn assert_nothing_changed(&self, before: &[(String, Vec<u8>)], text: &str) {
        let after = self.snapshot();
        let names = |s: &[(String, Vec<u8>)]| s.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(&after), names(before), "a path moved:\n{text}");
        assert!(after == before, "a file's bytes changed:\n{text}");
        assert!(!self.hold.exists(), "the converge was held:\n{text}");
        let mutators = [
            "docker compose -f",
            "rsync ",
            "mount ",
            "umount ",
            "chattr ",
            "systemctl ",
        ];
        for c in self.calls() {
            let reads = c.contains(" config --services") || c.contains(" ps -a -q ");
            assert!(
                reads || !mutators.iter().any(|m| c.starts_with(m)),
                "a mutating command ran: `{c}`\n{text}"
            );
        }
    }

    /// A refusal: exit 78, the needle said, no plan, nothing changed.
    fn refused(&self, args: &[&str], needle: &str) {
        let before = self.snapshot();
        let r = self.run(args);
        let text = r.text();
        assert_eq!(r.code, 78, "{args:?} was not refused (78):\n{text}");
        assert!(text.contains(needle), "expected `{needle}` in:\n{text}");
        assert!(text.contains("Nothing was changed"), "{text}");
        assert!(
            !r.err.contains("plan-sha256:"),
            "a refusal rendered a plan:\n{text}"
        );
        self.assert_nothing_changed(&before, &text);
    }

    /// The index of the recorded call that ends with `suffix`.
    fn call_ending(&self, suffix: &str) -> usize {
        let calls = self.calls();
        calls
            .iter()
            .position(|c| c.ends_with(suffix))
            .unwrap_or_else(|| panic!("no call ending `{suffix}` in:\n{}", calls.join("\n")))
    }

    /// The call that verified the TEMPORARY fstab (the live one is
    /// verified first, at every run).
    fn new_tab_verify(&self) -> usize {
        self.call_at(&format!(
            "findmnt --verify --tab-file {}.move-forgejo-data.",
            self.fstab.display()
        ))
    }
}

// ---------------------------------------------------------------------------
// The plan.
// ---------------------------------------------------------------------------

#[test]
fn the_plan_is_deterministic_names_its_hash_and_changes_nothing() {
    let c = Case::new("plan");
    let before = c.snapshot();
    let a = c.run(&["--plan"]);
    assert_eq!(a.code, 0, "the plan did not render:\n{}", a.text());
    let b = c.run(&["--plan"]);
    assert_eq!(a.out, b.out, "two renders of one state differ");
    // The hash on stderr is the hash of the bytes on stdout.
    let f = c.root.join("plan-bytes");
    write_file(&f, &a.out);
    assert_eq!(a.plan_sha(), sha256_of(&f));
    c.assert_nothing_changed(&before, &a.text());
    let plan: serde_json::Value = serde_json::from_str(&a.out).expect("the plan is JSON");
    assert_eq!(plan["verb"], "move-forgejo-data");
    assert_eq!(plan["target"]["uuid"], TUUID);
    assert_eq!(plan["target"]["label"], "boss-data");
    assert_eq!(plan["target"]["directory_state"], "absent");
    // 64 GiB steps: a few bytes round up to one step.
    assert_eq!(plan["source"]["size_at_most_gib"], 64);
    let names: Vec<&str> = plan["source"]["top_level"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["git", "gitea", "ssh"]);
}

/// The review's point (e): the ACT is inside the hashed bytes — the
/// literal rsync argv, the rename target, the fstab line and options —
/// from the variables the write executes, so what is approved and what
/// runs cannot be two spellings.
#[test]
fn the_plan_names_the_act_the_write_executes() {
    let c = Case::new("plan-act");
    let r = c.run(&["--plan"]);
    assert_eq!(r.code, 0, "{}", r.text());
    let plan: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    let src = c.src.display().to_string();
    let tgt = c.tgt().display().to_string();
    assert_eq!(
        plan["copy"],
        serde_json::json!([
            "rsync",
            "-aHAXS",
            "--numeric-ids",
            "--delete",
            format!("{src}/"),
            format!("{tgt}/")
        ])
    );
    assert_eq!(
        plan["verify"],
        serde_json::json!([
            "rsync",
            "-aHAXSc",
            "--numeric-ids",
            "--delete",
            "--dry-run",
            "--itemize-changes",
            format!("{src}/"),
            format!("{tgt}/")
        ])
    );
    assert_eq!(plan["rename_to"], c.pre().display().to_string());
    assert_eq!(plan["fstab_line"], c.fstab_line());
    let line = c.fstab_line();
    for opt in [
        "nofail",
        "x-systemd.device-timeout=",
        "x-systemd.requires-mounts-for=",
    ] {
        assert!(line.contains(opt), "the bind line lacks {opt}: {line}");
    }
    // The steps name `mount <path>`, never `mount -a`.
    let act = plan["act"].to_string();
    assert!(
        act.contains(&format!("mount --fstab {} {src}", c.fstab.display())),
        "{act}"
    );
    assert!(!act.contains("mount -a"), "{act}");

    // And the write ran exactly those argvs.
    let hash = r.plan_sha();
    let w = c.run(&[&hash]);
    assert_eq!(w.code, 0, "{}", w.text());
    let calls = c.calls();
    assert!(calls.contains(&format!(
        "rsync -aHAXS --numeric-ids --delete {src}/ {tgt}/"
    )));
    assert!(calls.contains(&format!(
        "rsync -aHAXSc --numeric-ids --delete --dry-run --itemize-changes {src}/ {tgt}/"
    )));
    // The stop is bounded, and the plan says which restart policy the
    // container runs under — the policy that could restart it mid-copy.
    assert!(calls.iter().any(|l| l.ends_with(" stop -t 60 forgejo")));
    assert_eq!(plan["forgejo"]["restart_policy"], "always");
    assert_eq!(plan["target"]["served_marker"], format!("{tgt}.went-live"));
}

/// No clock, no run id, no container id and no kernel device name in
/// the template: two renders of one state must be byte-identical, and
/// kernel names move when a disk is added.
#[test]
fn the_plan_templates_carry_no_clock_and_no_kernel_name() {
    for t in [
        "infra/forge/move-forgejo-data.plan.jq",
        "infra/forge/move-forgejo-data.rollback.plan.jq",
        "infra/forge/move-forgejo-data.restart.plan.jq",
    ] {
        let body = std::fs::read_to_string(repo_root().join(t)).unwrap();
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        for banned in ["now", "$__loc__", "date", "$cid", "$dev", "/dev/", "env."] {
            assert!(!code.contains(banned), "{t} carries `{banned}`");
        }
    }
}

// ---------------------------------------------------------------------------
// The signed write.
// ---------------------------------------------------------------------------

#[test]
fn the_signed_hash_moves_the_data_verifies_it_and_proves_forgejo_serves_it() {
    let c = Case::new("move");
    let fstab_before = c.fstab_text();
    let hash = c.run(&["--plan"]).plan_sha();
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 0, "the move failed:\n{text}");

    // The data is on the target, the old copy is the pre-move name, the
    // path is an empty mountpoint, and fstab is the old one plus the line.
    assert!(
        c.tgt().join("gitea/packages/ab/cd/blob").is_file(),
        "{text}"
    );
    assert!(
        c.pre().join("gitea/packages/ab/cd/blob").is_file(),
        "{text}"
    );
    assert_eq!(std::fs::read_dir(&c.src).unwrap().count(), 0, "{text}");
    assert_eq!(
        c.fstab_text(),
        format!("{fstab_before}{}\n", c.fstab_line())
    );
    assert!(
        std::fs::read_to_string(&c.mounts)
            .unwrap()
            .lines()
            .any(|l| l == c.src.display().to_string()),
        "{text}"
    );
    assert!(
        !c.hold.exists(),
        "the move's own hold was not released:\n{text}"
    );

    // The order, as the commands actually ran.
    let src = c.src.display().to_string();
    let stat = "docker exec -u git cid-forgejo-0001 stat -c %d:%i";
    let order = [
        c.call_at("curl http://forge.test:3000/api/healthz"),
        c.call_at("docker exec -u git"),
        // The served-copy leg is read in the before half too, against
        // the source, while nothing has changed (re-review finding 2).
        c.call_at(stat),
        c.call_at("curl http://forge.test:3000/v2/david/boss/manifests/"),
        c.call_ending(" stop -t 60 forgejo"),
        c.call_at("rsync -aHAXS "),
        c.call_at("rsync -aHAXSc "),
        c.new_tab_verify(),
        c.call_at(&format!("chattr +i -- {src}")),
        c.call_at("systemctl daemon-reload"),
        c.call_at(&format!("mount --fstab {} {src}", c.fstab.display())),
        c.call_ending(" start forgejo"),
        c.call_last(stat),
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "the calls ran out of order: {order:?}\n{}",
        c.calls().join("\n")
    );
    assert!(
        !c.calls().iter().any(|l| l.starts_with("mount -a")),
        "mount -a ran"
    );

    // Counts both sides, the proof, and the root df, on the record.
    for needle in [
        "per top-level entry (name, entries, bytes)",
        "counts equal the source's entry by entry",
        &format!("main {MAIN} before and after"),
        "manifest",
        "/ free:",
        "the container's /data is the moved copy",
        "restart policy: always",
    ] {
        assert!(
            text.contains(needle),
            "the output lacks `{needle}`:\n{text}"
        );
    }
    // The credential never reaches the output.
    for secret in ["ZGF2aWQ6c3R1Yi10b2tlbg==", "stub-token", "stub-bearer"] {
        assert!(
            !text.contains(secret),
            "the credential reached the output:\n{text}"
        );
    }
    // The marker records what the rollback must prove: this plan, the
    // stamp, and the sha256 of the manifest the before half read.
    let body = c.root.join("manifest-body");
    write_file(&body, "manifest-body");
    assert_eq!(
        std::fs::read_to_string(c.marker()).unwrap(),
        format!(
            "plan {hash}\nstamp {STAMP}\nmanifest {}\n",
            sha256_of(&body)
        )
    );
}

/// A plan that moved since it was signed changes nothing.
#[test]
fn a_plan_that_moved_since_its_signature_changes_nothing() {
    let c = Case::new("drift");
    let hash = c.run(&["--plan"]).plan_sha();
    // A new top-level entry appears between the signature and the run.
    std::fs::create_dir_all(c.src.join("custom")).unwrap();
    std::fs::remove_file(&c.calls).unwrap();
    c.refused(&[&hash], "not the approved");
}

/// Before the rename, a failure puts everything back: Forgejo starts
/// again on the untouched source and the move's own hold is released,
/// exit 1 because something did change for a while.
#[test]
fn a_failed_copy_before_the_commit_point_restarts_forgejo_and_releases_the_hold() {
    for (name, k) in [
        ("copy", "STUB_RSYNC_FAIL"),
        ("verify", "STUB_VERIFY_DIFF"),
        ("newtab", "STUB_VERIFY_FAIL"),
    ] {
        let c = Case::new(&format!("restore-{name}"));
        let fstab_before = c.fstab_text();
        let hash = c.run(&["--plan"]).plan_sha();
        let c = c.with(k, if k == "STUB_VERIFY_FAIL" { "new" } else { "1" });
        let r = c.run(&[&hash]);
        let text = r.text();
        assert_eq!(r.code, 1, "{name}: not a failure (1):\n{text}");
        assert!(
            text.contains("started forgejo again on the untouched"),
            "{name}:\n{text}"
        );
        assert!(
            !c.hold.exists(),
            "{name}: the hold was not released:\n{text}"
        );
        assert_eq!(
            std::fs::read_to_string(&c.state).unwrap().trim(),
            "true",
            "{name}"
        );
        assert!(c.src.join("gitea/packages/ab/cd/blob").is_file(), "{name}");
        assert!(!c.pre().exists(), "{name}: renamed:\n{text}");
        assert_eq!(c.fstab_text(), fstab_before, "{name}: fstab moved");
        let calls = c.calls();
        assert!(
            !calls
                .iter()
                .any(|l| l.starts_with("mount ") || l.starts_with("chattr ")),
            "{name}: a post-commit command ran:\n{}",
            calls.join("\n")
        );
        let leftovers = std::fs::read_dir(c.fstab.parent().unwrap())
            .unwrap()
            .count();
        assert_eq!(leftovers, 1, "{name}: a temporary fstab was left");
    }
}

/// After the rename nothing is undone by guess: the state is printed,
/// the hold stays, exit 1, and the rollback verb is named.
#[test]
fn a_failure_after_the_commit_point_keeps_the_hold_and_names_the_rollback() {
    let c = Case::new("post-commit");
    let hash = c.run(&["--plan"]).plan_sha();
    let c = c.with("STUB_START_FAIL", "1");
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(
        c.hold.exists(),
        "the hold was released after the commit point:\n{text}"
    );
    assert!(c.pre().is_dir(), "{text}");
    for needle in [
        "what stands now",
        "roll-back-forgejo-data-move",
        "the converge is HELD",
    ] {
        assert!(text.contains(needle), "expected `{needle}`:\n{text}");
    }
}

/// Review finding 1 (HOLD, 2026-09-27): dockerd restarts a
/// restart:always container on its own — a daemon restart mid-copy —
/// and Forgejo then writes to files already checksummed. The container's
/// StartedAt and RestartCount are read at the stop and again right
/// before the rename; a moved one fails the move before anything is
/// renamed, and the source stays authoritative.
#[test]
fn a_restart_during_the_copy_is_caught_before_the_rename() {
    let c = Case::new("restart-midcopy");
    let fstab_before = c.fstab_text();
    let hash = c.run(&["--plan"]).plan_sha();
    let c = c.with("STUB_RESTART_MIDCOPY", "1");
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(text.contains("before the rename"), "{text}");
    assert!(text.contains("restart policy always"), "{text}");
    assert!(
        !c.pre().exists(),
        "renamed under a running Forgejo:\n{text}"
    );
    assert_eq!(c.fstab_text(), fstab_before);
    assert!(c.src.join("gitea/packages/ab/cd/blob").is_file());
    assert!(!c.hold.exists(), "{text}");
}

/// A `compose start` of a container that is somehow already running is
/// a no-op: the start must be a real start (StartedAt moves), or the
/// proof would be read off whatever was running before.
#[test]
fn a_start_that_does_not_start_fails_and_stops_forgejo() {
    let c = Case::new("start-noop");
    let hash = c.run(&["--plan"]).plan_sha();
    let c = c.with("STUB_START_NOOP", "1");
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(text.contains("not a real start"), "{text}");
    assert!(c.hold.exists(), "{text}");
}

/// The healthz, main and manifest legs read alike off the old copy and
/// the new one, so they cannot say WHICH copy Forgejo serves. This leg
/// can: the device and inode of the repository as the container sees
/// it must be the moved copy's, never the pre-move copy's. A failed
/// proof stops Forgejo (finding 5) and keeps the hold.
#[test]
fn the_proof_ties_the_container_to_the_moved_copy_and_a_failed_proof_stops_forgejo() {
    let c = Case::new("serves-old");
    let hash = c.run(&["--plan"]).plan_sha();
    let old = c.pre();
    let c = c.with("STUB_DATA_ROOT", &old.display().to_string());
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(text.contains("not the moved copy"), "{text}");
    assert_eq!(
        std::fs::read_to_string(&c.state).unwrap().trim(),
        "false",
        "a failed proof left Forgejo running:\n{text}"
    );
    assert!(c.hold.exists(), "{text}");
    // And a main that reads differently after the start fails the same way.
    let c = Case::new("main-moved");
    let hash = c.run(&["--plan"]).plan_sha();
    let c = c.with("STUB_MAIN", STAMP);
    let r = c.run(&[&hash]);
    assert_eq!(r.code, 1, "{}", r.text());
    assert!(r.text().contains("refs/heads/main reads"), "{}", r.text());
    assert_eq!(std::fs::read_to_string(&c.state).unwrap().trim(), "false");
}

/// Finding 6: a HOST process (not a container) holding something under
/// the source after the stop fails the move before the copy, naming it.
#[test]
fn a_host_process_under_the_source_is_caught_before_the_copy() {
    let c = Case::new("host-holder");
    let hash = c.run(&["--plan"]).plan_sha();
    let pid = c.root.join("proc/4242");
    std::fs::create_dir_all(&pid).unwrap();
    std::os::unix::fs::symlink(c.src.join("gitea"), pid.join("cwd")).unwrap();
    write_file(&pid.join("comm"), "tar\n");
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(text.contains("4242 tar"), "{text}");
    assert!(!c.calls().iter().any(|l| l.starts_with("rsync ")), "{text}");
    assert!(
        text.contains("started forgejo again on the untouched"),
        "{text}"
    );
}

/// Finding 4: after a rollback, the moved copy holds what Forgejo wrote
/// while it was served, and a new move's `--delete` mirror would wipe
/// it. A target the move once put live carries a marker, and a move
/// onto it is refused.
#[test]
fn a_move_after_a_rollback_is_refused_rather_than_mirrored_over() {
    let c = Case::new("move-after-rollback");
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    let p = c.run(&["--plan-rollback"]);
    assert_eq!(c.run(&["--rollback", &p.plan_sha()]).code, 0);
    assert!(PathBuf::from(format!("{}.went-live", c.tgt().display())).is_file());
    std::fs::remove_file(&c.calls).unwrap();
    c.refused(&["--plan"], "was served by Forgejo");
}

/// Finding 4, the reviewer's own shape: a target holding anything newer
/// than the source's newest file is refused, whatever marker it has.
#[test]
fn a_target_newer_than_the_source_is_refused() {
    let c = Case::new("newer-target");
    put(&c.tgt().join("gitea/written-later"), "x\n");
    let ok = Command::new("touch")
        .args(["-d", "2099-01-01"])
        .arg(c.tgt().join("gitea/written-later"))
        .status()
        .unwrap()
        .success();
    assert!(ok);
    c.refused(&["--plan"], "newer than anything in");
}

// ---------------------------------------------------------------------------
// The re-review of the released car (backlog 85d29e33, 2026-09-27): a
// proof must not stop the forge for a reason of its own, and a stopped
// forge must have an approved way back.
// ---------------------------------------------------------------------------

/// Finding 2: the served-copy leg (`docker exec … stat`) was first run
/// after the commit point, so a Forgejo image with no stat — or one
/// whose stat prints otherwise — stopped the forge on the move's proof.
/// It is read in the before half against the source, as a refusal.
#[test]
fn the_served_copy_leg_is_proved_before_anything_is_touched() {
    for (name, k) in [("no-stat", "STUB_STAT_FAIL"), ("odd-stat", "STUB_STAT_ODD")] {
        let c = Case::new(&format!("serves-before-{name}"));
        let hash = c.run(&["--plan"]).plan_sha();
        std::fs::remove_file(&c.calls).unwrap();
        let c = c.with(k, "1");
        c.refused(&[&hash], "served-copy leg");
        assert_eq!(c.running(), "true", "{name}: Forgejo was touched");
    }
}

/// Finding 3: the marker is written before the rename, so a failure
/// after the rename (here: the mountpoint cannot be made immutable)
/// still leaves the target marked — a restart:always Forgejo may write
/// there from the rename on.
#[test]
fn the_served_marker_is_written_before_the_rename() {
    let c = Case::new("marker-before-rename");
    let hash = c.run(&["--plan"]).plan_sha();
    let c = c.with("STUB_CHATTR_FAIL", "1");
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(c.pre().is_dir(), "the rename did not happen:\n{text}");
    let record = std::fs::read_to_string(c.marker())
        .unwrap_or_else(|e| panic!("no marker after the rename ({e}):\n{text}"));
    assert!(
        record.starts_with(&format!("plan {hash}\nstamp {STAMP}\nmanifest ")),
        "{record}"
    );
}

/// ...and a failure BEFORE the rename removes the marker this run wrote:
/// Forgejo never served the target, so a retry must not be refused.
#[test]
fn a_rename_that_fails_removes_the_marker_it_wrote() {
    let c = Case::new("rename-fails");
    let hash = c.run(&["--plan"]).plan_sha();
    write_exec(&c.bin.join("mv"), STUB_MV_RENAME_FAILS);
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(!c.pre().exists(), "{text}");
    assert!(
        !c.marker().exists(),
        "the marker outlived a failed rename:\n{text}"
    );
    assert!(text.contains("the rename never happened"), "{text}");
    assert_eq!(c.running(), "true", "{text}");
    assert!(!c.hold.exists(), "{text}");
    // And the retry plans rather than refusing the target as served.
    std::fs::remove_file(c.bin.join("mv")).unwrap();
    let p = c.run(&["--plan"]);
    assert_eq!(p.code, 0, "{}", p.text());
}

/// Finding 6: a Forgejo that dockerd starts behind the verb's back once
/// the source is renamed is STOPPED, not left serving a state the run
/// cannot vouch for; the hold stays and the restart verb is named.
#[test]
fn a_start_behind_the_verbs_back_after_the_rename_stops_forgejo() {
    let c = Case::new("restart-after-rename");
    let hash = c.run(&["--plan"]).plan_sha();
    let c = c.with("STUB_RESTART_AT_MOUNT", "1");
    let r = c.run(&[&hash]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(text.contains("before the start"), "{text}");
    assert_eq!(
        c.running(),
        "false",
        "left running after the rename:\n{text}"
    );
    assert!(c.hold.exists(), "{text}");
    assert!(text.contains("restart-forgejo"), "{text}");
    assert!(
        !c.calls().iter().any(|l| l.ends_with(" start forgejo")),
        "the verb started it:\n{text}"
    );
}

/// Finding 1: after a later converge the stamp names an image pushed
/// into the moved copy, which the copy put back does not hold. The
/// rollback proves the stamp the MOVE recorded, off the very copy it
/// puts back, and succeeds — where proving the current stamp would 404,
/// stop Forgejo, and leave no verb to start it.
#[test]
fn the_rollback_proves_the_stamp_the_move_recorded() {
    let c = Case::new("rb-later-converge");
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    let later = "d00dfeed0000000000000000000000000000beef";
    write_file(&c.root.join("stamp"), &format!("{later}\n"));
    let c = c.with("STUB_MISSING_TAG", later);
    let p = c.run(&["--plan-rollback"]);
    assert_eq!(p.code, 0, "{}", p.text());
    let plan: serde_json::Value = serde_json::from_str(&p.out).unwrap();
    assert_eq!(plan["registry_record"]["stamp"], STAMP, "{}", p.out);
    assert_eq!(
        plan["registry_record"]["file"],
        c.marker().display().to_string()
    );
    std::fs::remove_file(&c.calls).unwrap();
    let r = c.run(&["--rollback", &p.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 0, "{text}");
    assert_eq!(c.running(), "true", "{text}");
    let calls = c.calls();
    assert!(
        calls
            .iter()
            .any(|l| l.ends_with(&format!("/manifests/{STAMP}"))),
        "{}",
        calls.join("\n")
    );
    assert!(
        !calls.iter().any(|l| l.contains(later)),
        "the rollback read the current stamp:\n{}",
        calls.join("\n")
    );
}

/// A marker that is not exactly the record a move writes says nothing
/// the rollback can prove against: refused, nothing changed.
#[test]
fn the_rollback_refuses_a_record_it_cannot_read() {
    let c = Case::new("rb-bad-record");
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    write_file(&c.marker(), &format!("{hash}\n"));
    std::fs::remove_file(&c.calls).unwrap();
    c.refused(&["--plan-rollback"], "does not read as the record");
}

/// Finding 4: with the drive gone, only an UNREACHABLE source under the
/// drive's own targets is tolerated. A parse error (which no target
/// carries) or any other error kind is refused.
#[test]
fn the_rollback_tolerates_only_an_unreachable_drive() {
    for kind in ["parse", "notreach"] {
        let c = Case::new(&format!("rb-verify-{kind}"));
        let hash = c.run(&["--plan"]).plan_sha();
        assert_eq!(c.run(&[&hash]).code, 0);
        write_file(&c.mounts, "/\n");
        c.set_lsblk(&[(
            "/dev/nvme1n1p2",
            None,
            ROOT_UUID,
            "ext4",
            vec!["/".to_string()],
        )]);
        let c = c.with("STUB_VERIFY_FAIL", kind);
        std::fs::remove_file(&c.calls).unwrap();
        c.refused(&["--plan-rollback"], "does not verify");
    }
}

// ---------------------------------------------------------------------------
// restart-forgejo: the approved way back for a Forgejo a proof stopped.
// ---------------------------------------------------------------------------

/// A Forgejo stopped on the untouched source (a directory on /) starts
/// again under a signed plan; the start is real and healthz passes.
#[test]
fn restart_forgejo_starts_a_stopped_forge_on_the_source() {
    let c = Case::new("restart-plain");
    write_file(&c.state, "false\n");
    let before = c.snapshot();
    let p = c.run(&["--plan-restart"]);
    assert_eq!(p.code, 0, "{}", p.text());
    c.assert_nothing_changed(&before, &p.text());
    let plan: serde_json::Value = serde_json::from_str(&p.out).unwrap();
    assert_eq!(plan["verb"], "restart-forgejo");
    assert_eq!(plan["source"]["mounted"], false);
    assert_eq!(plan["forgejo"]["running"], false);
    // Nothing a move left behind: the plan says so rather than omitting it
    // (backlog ed7702c3, finding 2).
    assert_eq!(plan["pre_move_copies"], serde_json::json!([]), "{}", p.out);
    assert_eq!(plan["served_marker"], serde_json::Value::Null, "{}", p.out);
    assert_eq!(plan["converge_hold"]["holds"], serde_json::Value::Null);
    assert_eq!(plan["unfinished_run"], serde_json::Value::Null);
    let r = c.run(&["--restart", &p.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 0, "{text}");
    assert_eq!(c.running(), "true", "{text}");
    // The plan's own prose names FINDING as a rule; a reported one is
    // the dash-led line.
    assert!(!text.contains("FINDING —"), "{text}");
    assert!(text.contains("OK — forgejo runs on"), "{text}");
    // A plan that no longer holds (it runs now) changes nothing.
    std::fs::remove_file(&c.calls).unwrap();
    c.refused(&["--restart", &p.plan_sha()], "is running");
}

/// The case it exists for: a move whose proof failed, leaving Forgejo
/// stopped on the mounted bind with the hold kept. The restart names the
/// bind, starts Forgejo, and leaves the hold as it found it.
#[test]
fn restart_forgejo_recovers_a_move_whose_proof_stopped_the_forge() {
    let c = Case::new("restart-after-proof");
    let hash = c.run(&["--plan"]).plan_sha();
    let old = c.pre().display().to_string();
    let src = c.src.display().to_string();
    let c = c.with("STUB_DATA_ROOT", &old);
    assert_eq!(c.run(&[&hash]).code, 1);
    assert_eq!(c.running(), "false");
    assert!(c.hold.exists());
    // The fake has no real bind: show at the source what the bind would.
    let ok = Command::new("cp")
        .arg("-a")
        .arg(format!("{}/.", c.tgt().display()))
        .arg(&c.src)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let c = c.with("STUB_DATA_ROOT", &src);
    let p = c.run(&["--plan-restart"]);
    assert_eq!(p.code, 0, "{}", p.text());
    let plan: serde_json::Value = serde_json::from_str(&p.out).unwrap();
    assert_eq!(plan["source"]["mounted"], true);
    assert_eq!(plan["source"]["mount_uuid"], TUUID);
    // What the approver is starting Forgejo on, in the signed bytes
    // (backlog ed7702c3, finding 2): the pre-move copy still stands, the
    // moved copy's marker records the move that put it live, and the
    // move's own hold says that move never passed its proof.
    assert_eq!(
        plan["pre_move_copies"],
        serde_json::json!([old]),
        "{}",
        p.out
    );
    assert_eq!(
        plan["served_marker"]["file"],
        c.marker().display().to_string()
    );
    assert_eq!(plan["served_marker"]["record"]["plan"], hash, "{}", p.out);
    assert_eq!(plan["served_marker"]["record"]["stamp"], STAMP);
    assert_eq!(
        plan["converge_hold"]["holds"],
        format!("forgejo-data-move-{}", &hash[..12]),
        "{}",
        p.out
    );
    assert!(
        plan["unfinished_run"]
            .as_str()
            .is_some_and(|s| s.contains("never passed")),
        "{}",
        p.out
    );
    let r = c.run(&["--restart", &p.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 0, "{text}");
    assert_eq!(c.running(), "true", "{text}");
    assert!(c.hold.exists(), "the restart released a hold:\n{text}");
    assert!(text.contains("still held"), "{text}");
}

/// Every state but the two the move and the rollback leave is refused,
/// by the plan and by the write alike, with nothing changed.
#[test]
fn restart_forgejo_refuses_every_other_state() {
    type Setup = fn(&Case) -> Vec<(&'static str, String)>;
    let cases: Vec<(&str, Setup, &str)> = vec![
        ("running", |_| vec![], "is running"),
        (
            "absent",
            |c| {
                write_file(&c.state, "false\n");
                std::fs::rename(&c.src, c.pre()).unwrap();
                vec![]
            },
            "does not exist",
        ),
        (
            "empty-mountpoint",
            |c| {
                write_file(&c.state, "false\n");
                std::fs::rename(&c.src, c.pre()).unwrap();
                std::fs::create_dir_all(&c.src).unwrap();
                vec![]
            },
            "is empty",
        ),
        (
            "foreign-mount",
            |c| {
                write_file(&c.state, "false\n");
                let mut m = std::fs::read_to_string(&c.mounts).unwrap();
                m.push_str(&format!("{}\n", c.src.display()));
                write_file(&c.mounts, &m);
                vec![("STUB_BIND_ROOT", "/".into())]
            },
            "not the /forgejo-data bind",
        ),
        (
            "no-repository",
            |c| {
                write_file(&c.state, "false\n");
                std::fs::remove_dir_all(c.src.join("git")).unwrap();
                vec![]
            },
            "the repository the proof reads",
        ),
    ];
    for (name, setup, needle) in cases {
        let c = Case::new(&format!("restart-refuse-{name}"));
        let extra = setup(&c);
        let mut c = c;
        for (k, v) in extra {
            c = c.with(k, &v);
        }
        c.refused(&["--plan-restart"], needle);
        c.refused(&["--restart", &"0".repeat(64)], needle);
    }
}

/// The restart never stops Forgejo: a start that is not real fails
/// (exit 1) without a stop, and a proof leg that cannot be read is a
/// FINDING that leaves Forgejo running — and the run RED, exit 1, never
/// an OK: green with a finding is a check nobody reads (review of the
/// released follow-up car, backlog ed7702c3, finding 2).
#[test]
fn restart_forgejo_never_stops_forgejo() {
    let c = Case::new("restart-noop");
    write_file(&c.state, "false\n");
    let p = c.run(&["--plan-restart"]);
    let c = c.with("STUB_START_NOOP", "1");
    let r = c.run(&["--restart", &p.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(text.contains("not a real start"), "{text}");
    assert!(
        !c.calls().iter().any(|l| l.ends_with(" stop -t 60 forgejo")),
        "the restart stopped forgejo:\n{text}"
    );

    for (name, k, v, needle) in [
        (
            "stat",
            "STUB_STAT_FAIL",
            "1",
            "FINDING — the served-copy leg",
        ),
        ("main", "STUB_MAIN", "not-a-sha", "FINDING — git inside"),
    ] {
        let c = Case::new(&format!("restart-finding-{name}"));
        write_file(&c.state, "false\n");
        let p = c.run(&["--plan-restart"]);
        let c = c.with(k, v);
        let r = c.run(&["--restart", &p.plan_sha()]);
        let text = r.text();
        assert_eq!(r.code, 1, "{name}: a FINDING read green:\n{text}");
        assert!(text.contains(needle), "{name}:\n{text}");
        assert!(text.contains("started, not proven"), "{name}:\n{text}");
        assert!(!text.contains("OK —"), "{name}:\n{text}");
        assert_eq!(c.running(), "true", "{name}: {text}");
        assert!(
            !c.calls().iter().any(|l| l.ends_with(" stop -t 60 forgejo")),
            "{name}: the restart stopped forgejo:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The review of the released follow-up car (backlog ed7702c3,
// 2026-09-27): the commit point is the rename whatever the stage says, a
// stale marker is read at plan time, and every plan says where its
// repository path came from.
// ---------------------------------------------------------------------------

/// Finding 1: a TERM, INT or HUP landing after `mv -T SRC PRE` returned
/// and before `STAGE=committed` ran the EXIT trap's pre-rename branch:
/// `compose start` on a source that no longer existed (compose binds
/// create a missing host path, so a fresh EMPTY Forgejo on the root
/// disk), the hold released, and the marker of a run that DID rename
/// removed. The signal here is delivered by the `mv` that made the
/// rename, so it lands in exactly that window.
#[test]
fn a_signal_after_the_rename_leaves_the_commit_point_standing() {
    for sig in ["TERM", "INT", "HUP"] {
        let c = Case::new(&format!("signal-after-rename-{}", sig.to_lowercase()));
        let hash = c.run(&["--plan"]).plan_sha();
        write_exec(&c.bin.join("mv"), STUB_MV_SIGNAL_AFTER_RENAME);
        let c = c.with("STUB_SIGNAL", sig);
        let r = c.run(&[&hash]);
        let text = r.text();
        assert_eq!(r.code, 1, "{sig}:\n{text}");
        assert!(
            c.pre().join("gitea/packages/ab/cd/blob").is_file(),
            "{sig}: the rename did not happen, so this case tests nothing:\n{text}"
        );
        assert!(
            !c.calls().iter().any(|l| l.ends_with(" start forgejo")),
            "{sig}: Forgejo was started on a source that was renamed away:\n{text}"
        );
        assert_eq!(c.running(), "false", "{sig}:\n{text}");
        assert!(!c.src.exists(), "{sig}: the source was made again:\n{text}");
        assert!(
            c.hold.exists(),
            "{sig}: the hold was released past the commit point:\n{text}"
        );
        assert!(
            c.marker().is_file(),
            "{sig}: the marker of a run that renamed was removed:\n{text}"
        );
        assert!(
            !text.contains("the rename never happened"),
            "{sig}:\n{text}"
        );
        assert!(
            text.contains("roll-back-forgejo-data-move"),
            "{sig}:\n{text}"
        );
    }
}

/// Finding 4: the served marker was read only when the target directory
/// stood, so a marker whose target was deleted passed the plan and was
/// found only by the write's re-check after the ~100 GB copy. It is read
/// at plan time either way — and a dangling link counts as a marker.
#[test]
fn a_served_marker_is_refused_at_plan_time_even_without_its_target() {
    let c = Case::new("marker-no-target");
    put(&c.marker(), &format!("plan {}\n", "0".repeat(64)));
    assert!(!c.tgt().exists());
    c.refused(&["--plan"], "was served by Forgejo");
    let c = Case::new("marker-link-no-target");
    std::os::unix::fs::symlink(c.root.join("gone"), c.marker()).unwrap();
    c.refused(&["--plan"], "was served by Forgejo");
}

/// Finding 5: the repository the proof reads is found by layers
/// (forge-repo-path.sh), one of which is an override from the
/// environment. Every plan carries WHICH layer answered, so a path that
/// was named rather than derived is in the bytes the approver signs.
#[test]
fn every_plan_says_where_its_repository_path_came_from() {
    let c = Case::new("repo-from");
    let repo = c
        .src
        .join("git/repositories/david/boss.git")
        .display()
        .to_string();
    let m = c.run(&["--plan"]);
    let plan: serde_json::Value = serde_json::from_str(&m.out).unwrap();
    assert_eq!(plan["repository"]["host_path"], repo, "{}", m.out);
    assert_eq!(
        plan["repository"]["in_container"],
        "/data/git/repositories/david/boss.git"
    );
    assert!(
        plan["repository"]["from"]
            .as_str()
            .is_some_and(|s| s.starts_with("derived: ")),
        "{}",
        m.out
    );
    assert_eq!(c.run(&[&m.plan_sha()]).code, 0);

    let p = c.run(&["--plan-rollback"]);
    let plan: serde_json::Value = serde_json::from_str(&p.out).unwrap();
    assert!(
        plan["repository"]["from"]
            .as_str()
            .is_some_and(|s| s.starts_with("derived: ")),
        "{}",
        p.out
    );
    let c = c.with("BOSS_FORGE_REPO_PATH", &repo);
    let q = c.run(&["--plan-rollback"]);
    let plan: serde_json::Value = serde_json::from_str(&q.out).unwrap();
    assert_eq!(plan["repository"]["host_path"], repo);
    assert_eq!(
        plan["repository"]["from"], "BOSS_FORGE_REPO_PATH in the environment",
        "{}",
        q.out
    );
    assert_ne!(p.plan_sha(), q.plan_sha(), "the layer is not in the bytes");

    let c = Case::new("repo-from-restart");
    write_file(&c.state, "false\n");
    let r = c.run(&["--plan-restart"]);
    let plan: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert!(
        plan["repository"]["from"]
            .as_str()
            .is_some_and(|s| s.starts_with("derived: ")),
        "{}",
        r.out
    );
}

// ---------------------------------------------------------------------------
// Every refusal changes nothing.
// ---------------------------------------------------------------------------

#[test]
fn every_precondition_is_a_refusal_that_changes_nothing() {
    type Setup = fn(&Case) -> Vec<(&'static str, String)>;
    let none: Setup = |_| vec![];
    let cases: Vec<(&str, Setup, &str)> = vec![
        (
            "symlink",
            |c| {
                let real = c.root.join("elsewhere");
                std::fs::rename(&c.src, &real).unwrap();
                std::os::unix::fs::symlink(&real, &c.src).unwrap();
                vec![]
            },
            "is a symlink",
        ),
        (
            "is-a-mount",
            |c| {
                let mut m = std::fs::read_to_string(&c.mounts).unwrap();
                m.push_str(&format!("{}\n", c.src.display()));
                write_file(&c.mounts, &m);
                vec![]
            },
            "is itself a mount",
        ),
        (
            "not-on-root",
            |_| vec![("STUB_SRC_ON", "/opt".into())],
            "not on /",
        ),
        (
            "mount-beneath",
            |c| {
                let mut m = std::fs::read_to_string(&c.mounts).unwrap();
                m.push_str(&format!("{}/gitea/packages\n", c.src.display()));
                write_file(&c.mounts, &m);
                vec![]
            },
            "mounted beneath",
        ),
        (
            "compose-binds-another",
            |c| {
                put(
                    &c.root.join("opt/forgejo/docker-compose.yml"),
                    "services:\n  forgejo:\n    volumes:\n      - /srv/other:/data\n",
                );
                vec![]
            },
            "binds /srv/other at /data",
        ),
        (
            "not-running",
            |c| {
                write_file(&c.state, "false\n");
                vec![]
            },
            "is not running",
        ),
        (
            "another-container",
            |c| {
                vec![
                    ("STUB_OTHER_ID", "cid-runner-0002".into()),
                    ("STUB_OTHER_MOUNT", format!("{}/gitea", c.src.display())),
                ]
            },
            "another running container mounts",
        ),
        (
            // Re-review finding 5: a container binding a directory ABOVE
            // the source sees it too.
            "parent-container",
            |c| {
                vec![
                    ("STUB_OTHER_ID", "cid-runner-0002".into()),
                    (
                        "STUB_OTHER_MOUNT",
                        c.root.join("opt/forgejo").display().to_string(),
                    ),
                ]
            },
            "another running container mounts",
        ),
        (
            // Re-review finding 5: a process table that cannot be read
            // would name no holder at all — refused, never an empty table.
            "proc-unreadable",
            |c| {
                std::fs::remove_dir_all(c.root.join("proc/self")).unwrap();
                vec![]
            },
            "process table",
        ),
        (
            "two-containers",
            |_| vec![("STUB_CIDS", "a\nb\n".into())],
            "not exactly one",
        ),
        (
            "no-label",
            |c| {
                c.set_lsblk(&[("/dev/nvme1n1p2", None, ROOT_UUID, "ext4", vec!["/".into()])]);
                vec![]
            },
            "0 filesystems are labelled boss-data",
        ),
        (
            "two-labels",
            |c| {
                let m = c.mnt.display().to_string();
                c.set_lsblk(&[
                    ("/dev/a", Some("boss-data"), TUUID, "ext4", vec![m]),
                    ("/dev/b", Some("boss-data"), "1111", "ext4", vec![]),
                ]);
                vec![]
            },
            "2 filesystems are labelled boss-data",
        ),
        (
            "not-ext4",
            |c| {
                c.set_lsblk(&[(
                    "/dev/a",
                    Some("boss-data"),
                    TUUID,
                    "xfs",
                    vec![c.mnt.display().to_string()],
                )]);
                vec![]
            },
            "not the ext4",
        ),
        (
            "unmounted",
            |c| {
                c.set_lsblk(&[("/dev/a", Some("boss-data"), TUUID, "ext4", vec![])]);
                vec![]
            },
            "mounted 0 times",
        ),
        (
            "at-root",
            |c| {
                c.set_lsblk(&[("/dev/a", Some("boss-data"), TUUID, "ext4", vec!["/".into()])]);
                vec![]
            },
            "mounted at /",
        ),
        (
            "dot-dot",
            |c| {
                let m = format!("{}/../boss-data", c.mnt.display());
                c.set_lsblk(&[("/dev/a", Some("boss-data"), TUUID, "ext4", vec![m])]);
                vec![]
            },
            "not a plain absolute path",
        ),
        (
            "is-root",
            |c| {
                c.set_lsblk(&[(
                    "/dev/a",
                    Some("boss-data"),
                    ROOT_UUID,
                    "ext4",
                    vec![c.mnt.display().to_string()],
                )]);
                vec![("STUB_TUUID", ROOT_UUID.into())]
            },
            "is the root filesystem",
        ),
        (
            "readings-disagree",
            |_| vec![("STUB_SEEN_UUID", "deadbeef-0000".into())],
            "two readings disagree",
        ),
        (
            "no-uuid-line",
            |c| {
                write_file(&c.fstab, &format!("UUID={ROOT_UUID} / ext4 defaults 0 1\n"));
                vec![]
            },
            "by-UUID mount",
        ),
        (
            "already-in-fstab",
            |c| {
                let mut t = c.fstab_text();
                t.push_str(&format!("/x {} none bind 0 0\n", c.src.display()));
                write_file(&c.fstab, &t);
                vec![]
            },
            "already has",
        ),
        (
            "fstab-unverified",
            |_| vec![("STUB_VERIFY_FAIL", "now".into())],
            "does not verify clean today",
        ),
        (
            "pre-move-exists",
            |c| {
                std::fs::create_dir_all(c.pre()).unwrap();
                vec![]
            },
            "already exists",
        ),
        (
            "too-small",
            |_| vec![("STUB_AVAIL", "10737418240".into())],
            "GiB margin needs",
        ),
        (
            "odd-name",
            |c| {
                std::fs::create_dir_all(c.src.join("a name")).unwrap();
                vec![]
            },
            "is not plain",
        ),
        (
            "empty",
            |c| {
                std::fs::remove_dir_all(&c.src).unwrap();
                std::fs::create_dir_all(&c.src).unwrap();
                vec![]
            },
            "is empty",
        ),
        (
            "no-login",
            |c| {
                std::fs::remove_file(c.root.join("docker-config.json")).unwrap();
                vec![]
            },
            "no docker config",
        ),
        (
            "no-stamp",
            |c| {
                std::fs::remove_file(c.root.join("stamp")).unwrap();
                vec![]
            },
            "stamp",
        ),
        (
            "findmnt-fails",
            |_| vec![("STUB_FINDMNT_FAIL", "1".into())],
            "findmnt could not",
        ),
        ("plain", none, ""),
    ];
    for (name, setup, needle) in cases {
        let c = Case::new(&format!("refuse-{name}"));
        let extra = setup(&c);
        let mut c = c;
        for (k, v) in extra {
            c = c.with(k, &v);
        }
        if name == "plain" {
            // The control: the same fixture, untouched, renders a plan.
            let r = c.run(&["--plan"]);
            assert_eq!(r.code, 0, "the control fixture did not plan:\n{}", r.text());
            continue;
        }
        c.refused(&["--plan"], needle);
        // The write refuses the same way, whatever hash it is handed.
        c.refused(&[&"0".repeat(64)], needle);
    }
}

/// Before the stop, the proof's before half is read; a forge already
/// unwell is a refusal, with nothing changed.
#[test]
fn an_unhealthy_forge_is_refused_before_anything_changes() {
    let c = Case::new("unhealthy");
    let hash = c.run(&["--plan"]).plan_sha();
    std::fs::remove_file(&c.calls).unwrap();
    let c = c.with("STUB_UNHEALTHY", "1");
    c.refused(&[&hash], "does not pass before anything is touched");
}

#[test]
fn the_arguments_are_refused_before_anything_is_read() {
    let c = Case::new("args");
    for (args, needle) in [
        (vec![], "usage"),
        (vec!["--plan", "extra"], "usage"),
        (vec!["--rollback"], "usage"),
        (vec!["--rollback", "abc"], "64 lowercase hex"),
        (vec!["--restart"], "usage"),
        (vec!["--plan-restart", "extra"], "usage"),
        (vec!["--restart", "abc"], "64 lowercase hex"),
        (vec!["/opt/forgejo/data"], "64 lowercase hex"),
        (vec![&*"A".repeat(64)], "64 lowercase hex"),
        (vec!["--for-real"], "64 lowercase hex"),
    ] {
        let r = c.run(&args);
        assert_eq!(r.code, 78, "{args:?}:\n{}", r.text());
        assert!(
            r.text().contains(needle),
            "{args:?}: expected {needle}:\n{}",
            r.text()
        );
        assert!(
            c.calls().is_empty(),
            "{args:?} read the host: {:?}",
            c.calls()
        );
    }
}

// ---------------------------------------------------------------------------
// The rollback.
// ---------------------------------------------------------------------------

#[test]
fn the_rollback_puts_a_finished_move_back_exactly() {
    let c = Case::new("rollback");
    let fstab_before = c.fstab_text();
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    std::fs::remove_file(&c.calls).unwrap();

    let p = c.run(&["--plan-rollback"]);
    assert_eq!(p.code, 0, "{}", p.text());
    let plan: serde_json::Value = serde_json::from_str(&p.out).unwrap();
    assert_eq!(plan["fstab_line"], c.fstab_line());
    assert_eq!(plan["source"]["mounted"], true);
    assert_eq!(plan["pre_move_copy"]["path"], c.pre().display().to_string());
    assert!(
        plan["act"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["applies"] == true),
        "a finished move needs every undo step: {}",
        plan["act"]
    );
    assert!(plan["effect"].as_str().unwrap().contains("NOT copied back"));

    let r = c.run(&["--rollback", &p.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 0, "the rollback failed:\n{text}");
    assert_eq!(
        c.fstab_text(),
        fstab_before,
        "the fstab line was not removed exactly"
    );
    assert!(c.src.join("gitea/packages/ab/cd/blob").is_file(), "{text}");
    assert!(!c.pre().exists(), "{text}");
    assert!(
        c.tgt().join("gitea/packages/ab/cd/blob").is_file(),
        "the moved copy was touched"
    );
    assert!(!c.hold.exists(), "{text}");
    let src = c.src.display().to_string();
    let order = [
        c.call_ending(" stop -t 60 forgejo"),
        c.call_at(&format!("umount -- {src}")),
        c.new_tab_verify(),
        c.call_at("systemctl daemon-reload"),
        c.call_at(&format!("chattr -i -- {src}")),
        c.call_ending(" start forgejo"),
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "{order:?}\n{}",
        c.calls().join("\n")
    );
}

/// A move that stopped right after its rename — no fstab line, nothing
/// mounted, Forgejo stopped — is repaired by the same verb: each undo
/// step applies only when the state needs it.
#[test]
fn the_rollback_repairs_a_move_that_stopped_after_its_rename() {
    let c = Case::new("rollback-partial");
    let fstab_before = c.fstab_text();
    std::fs::rename(&c.src, c.pre()).unwrap();
    write_file(&c.state, "false\n");
    let p = c.run(&["--plan-rollback"]);
    assert_eq!(p.code, 0, "{}", p.text());
    let plan: serde_json::Value = serde_json::from_str(&p.out).unwrap();
    assert_eq!(plan["fstab_line"], serde_json::Value::Null);
    assert_eq!(plan["source"]["state"], "absent");
    let applies: Vec<bool> = plan["act"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["applies"].as_bool().unwrap())
        .collect();
    assert_eq!(
        applies,
        [true, false, false, false, false, true, true, true, true]
    );
    let r = c.run(&["--rollback", &p.plan_sha()]);
    assert_eq!(r.code, 0, "{}", r.text());
    assert!(c.src.join("gitea/packages/ab/cd/blob").is_file());
    assert_eq!(c.fstab_text(), fstab_before);
    let calls = c.calls();
    assert!(
        !calls
            .iter()
            .any(|l| l.starts_with("umount ") || l.starts_with("chattr ")),
        "a step that did not apply ran:\n{}",
        calls.join("\n")
    );
}

#[test]
fn the_rollback_refuses_a_state_it_did_not_make() {
    // No pre-move copy.
    let c = Case::new("rb-none");
    c.refused(&["--plan-rollback"], "0 pre-move copies");
    // Two.
    let c = Case::new("rb-two");
    std::fs::create_dir_all(c.pre()).unwrap();
    std::fs::create_dir_all(format!("{}.pre-move-12345678", c.src.display())).unwrap();
    c.refused(&["--plan-rollback"], "2 pre-move copies");
    // An fstab line for the path that is not exactly the one the move writes.
    let c = Case::new("rb-foreign-line");
    std::fs::rename(&c.src, c.pre()).unwrap();
    let mut t = c.fstab_text();
    t.push_str(&c.fstab_line().replace("nofail,", ""));
    t.push('\n');
    write_file(&c.fstab, &t);
    c.refused(
        &["--plan-rollback"],
        "not exactly the line this verb writes",
    );
    // A mount at the path that is not the /forgejo-data bind.
    let c = Case::new("rb-foreign-mount");
    std::fs::rename(&c.src, c.pre()).unwrap();
    std::fs::create_dir_all(&c.src).unwrap();
    let mut m = std::fs::read_to_string(&c.mounts).unwrap();
    m.push_str(&format!("{}\n", c.src.display()));
    write_file(&c.mounts, &m);
    let c = c.with("STUB_BIND_ROOT", "/");
    c.refused(&["--plan-rollback"], "not the /forgejo-data bind");
    // An unmounted path that holds something is not the empty mountpoint.
    let c = Case::new("rb-full-path");
    std::fs::rename(&c.src, c.pre()).unwrap();
    put(&c.src.join("stray"), "x\n");
    c.refused(&["--plan-rollback"], "not the empty mountpoint");
}

/// Review finding 2: the rollback is needed MOST when the drive is gone,
/// and then findmnt --verify reports the drive's by-UUID line as an
/// unreachable required source (util-linux 2.38.1, even with nofail).
/// The rollback tolerates exactly that — errors under the drive's own
/// mount and the bind being removed, while the drive is absent — and
/// nothing else.
#[test]
fn the_rollback_proceeds_when_the_drive_is_gone() {
    let c = Case::new("rb-drive-gone");
    let fstab_before = c.fstab_text();
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    // A boot without the drive: nothing mounted but /, and no boss-data.
    write_file(&c.mounts, "/\n");
    c.set_lsblk(&[(
        "/dev/nvme1n1p2",
        None,
        ROOT_UUID,
        "ext4",
        vec!["/".to_string()],
    )]);
    let c = c.with("STUB_VERIFY_FAIL", "drive");
    let p = c.run(&["--plan-rollback"]);
    assert_eq!(p.code, 0, "{}", p.text());
    let plan: serde_json::Value = serde_json::from_str(&p.out).unwrap();
    assert_eq!(plan["drive"]["present"], false);
    assert_eq!(plan["drive"]["uuid"], TUUID);
    // The move's record of its stamp was on the drive: the registry leg
    // is reported, not proved, and the signed plan says so (re-review
    // finding 1) — never a proof that stops Forgejo on a 404.
    assert_eq!(plan["registry_record"], serde_json::Value::Null);
    assert!(
        plan["proof"]["registry"]
            .as_str()
            .unwrap()
            .starts_with("NOT PROVED, reported: the drive"),
        "{}",
        plan["proof"]
    );
    std::fs::remove_file(&c.calls).unwrap();
    let r = c.run(&["--rollback", &p.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 0, "{text}");
    assert!(text.contains("tolerated"), "{text}");
    assert!(
        text.contains("FINDING — the registry leg is not proved"),
        "{text}"
    );
    assert_eq!(c.running(), "true", "{text}");
    assert!(
        !c.calls().iter().any(|l| l.contains("/manifests/")),
        "{}",
        c.calls().join("\n")
    );
    assert_eq!(c.fstab_text(), fstab_before);
    assert!(c.src.join("gitea/packages/ab/cd/blob").is_file(), "{text}");
}

#[test]
fn the_rollback_tolerates_no_other_verify_error() {
    // An error on another target, with the drive gone: refused.
    let c = Case::new("rb-other-error");
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    write_file(&c.mounts, "/\n");
    c.set_lsblk(&[(
        "/dev/nvme1n1p2",
        None,
        ROOT_UUID,
        "ext4",
        vec!["/".to_string()],
    )]);
    let c = c.with("STUB_VERIFY_FAIL", "other");
    std::fs::remove_file(&c.calls).unwrap();
    c.refused(&["--plan-rollback"], "does not verify");
    // The drive's own errors while the drive is PRESENT: refused too.
    let c = Case::new("rb-drive-present");
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    let c = c.with("STUB_VERIFY_FAIL", "drive");
    std::fs::remove_file(&c.calls).unwrap();
    c.refused(&["--plan-rollback"], "does not verify");
}

/// Finding 6: an umount that answers EBUSY is retried, and each refusal
/// names who holds the mount (fuser -vm).
#[test]
fn a_busy_umount_is_retried_and_names_its_holders() {
    let c = Case::new("rb-busy");
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    let p = c.run(&["--plan-rollback"]);
    let c = c.with("STUB_UMOUNT_BUSY", "1");
    let r = c.run(&["--rollback", &p.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 0, "{text}");
    assert!(text.contains("stub fuser: 4242 git holds"), "{text}");
    // Busy past every retry: a failure that says what stands.
    let c = Case::new("rb-busy-forever");
    let hash = c.run(&["--plan"]).plan_sha();
    assert_eq!(c.run(&[&hash]).code, 0);
    let p = c.run(&["--plan-rollback"]);
    let c = c.with("STUB_UMOUNT_BUSY", "9");
    let r = c.run(&["--rollback", &p.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 1, "{text}");
    assert!(text.contains("three times"), "{text}");
    assert!(c.pre().is_dir(), "{text}");
}

// ---------------------------------------------------------------------------
// The file, read by line.
// ---------------------------------------------------------------------------

fn code_lines(rel: &str) -> Vec<String> {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn is_code(l: &str) -> bool {
    let t = l.trim_start();
    !t.is_empty() && !t.starts_with('#')
}

/// THE ORDER OF THE WRITE, read off the file. Each needle is a code line
/// in `move_write`; the pins read only code, because the header
/// describes every one of these commands in prose.
#[test]
fn the_move_compares_then_mutates_in_its_declared_order() {
    let lines = code_lines(SCRIPT);
    let body_from = lines
        .iter()
        .position(|l| l.starts_with("move_write() {"))
        .expect("move_write is gone");
    let body_to = body_from
        + lines[body_from..]
            .iter()
            .position(|l| l == "}")
            .expect("move_write never closes");
    let at = |needle: &str| -> usize {
        (body_from..body_to)
            .find(|&i| is_code(&lines[i]) && lines[i].contains(needle))
            .unwrap_or_else(|| panic!("move_write has no code line with {needle:?}"))
    };
    let compare = lines
        .iter()
        .position(|l| is_code(l) && l.contains("if [ \"$HASH\" != \"$APPROVED\" ]"))
        .expect("the compare is gone");
    assert!(compare < body_from, "the compare must precede the write");
    let order = [
        at("healthz_ok || refuse"),
        // The served-copy leg, proved against the source while nothing
        // has changed (re-review finding 2).
        at("serves_from \"$SRC\""),
        at("place_hold \"forgejo-data-move-"),
        at("STOPPED=1"),
        at("stop -t 60 \"$SERVICE\""),
        at("FACTS_STOPPED=$(container_facts"),
        // main's sha is read AFTER the stop, by host git, so a train
        // merging between a read and the stop cannot fail the proof.
        at("MAIN_BEFORE=$(host_main_sha"),
        at("holders_under \"$SRC\""),
        at("census \"$SRC\""),
        at("\"${COPY[@]}\""),
        at("\"${VERIFY[@]}\""),
        at("census \"$TGT\""),
        at("fstab_verifies \"$NEWTAB\""),
        // Still stopped, and not restarted in between: read again
        // immediately before the rename and again before the start.
        at("still_stopped 'before the rename'"),
        // The marker is down BEFORE the rename (re-review finding 3).
        at("> \"$MARKER\""),
        // Set immediately before the rename, so the EXIT trap reads the
        // tree for which side of it a signal landed on (backlog
        // ed7702c3, finding 1).
        at("RENAMING=1"),
        at("mv -T -- \"$SRC\" \"$PRE\""),
        at("STAGE=committed"),
        at("chattr +i"),
        at("mv -f -- \"$NEWTAB\" \"$FSTAB\""),
        at("systemctl daemon-reload"),
        at("mount --fstab \"$FSTAB\" \"$SRC\""),
        at("still_stopped 'before the start'"),
        at("start \"$SERVICE\""),
        at("real_start"),
        at("serves_from \"$TGT\""),
        at("release_own_hold"),
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "move_write's commands are out of their declared order (line numbers): {:?}",
        order.map(|i| i + 1)
    );
    // Before the first mutation every failure is a refusal (78); from
    // the hold on, a `refuse` would claim "nothing was changed" falsely.
    let first_mutation = at("place_hold \"forgejo-data-move-");
    assert!(
        at("STOPPED=1") < at("stop -t 60 \"$SERVICE\""),
        "STOPPED=1 must be set before the stop is asked for"
    );
    no_refusal_after(&lines, first_mutation, body_to, "move_write");
}

/// No `refuse` (exit 78, "nothing was changed") on a code line in
/// `lines[from + 1..to]`.
fn no_refusal_after(lines: &[String], from: usize, to: usize, what: &str) {
    for (i, l) in lines.iter().enumerate().take(to).skip(from + 1) {
        assert!(
            !(is_code(l) && l.contains("refuse ")),
            "{what}: line {} refuses (78) after the first mutation: {}",
            i + 1,
            l.trim()
        );
    }
}

/// The rollback's first mutation is its hold; from there a `refuse`
/// would print "Nothing was changed" over a stopped Forgejo, a removed
/// fstab line or a renamed copy (review of the released follow-up car,
/// backlog ed7702c3, finding 5 — the move carried this pin, the
/// rollback did not).
#[test]
fn the_rollback_never_refuses_after_its_first_mutation() {
    let lines = code_lines(SCRIPT);
    let from = lines
        .iter()
        .position(|l| l.starts_with("rollback_write() {"))
        .expect("rollback_write is gone");
    let to = from
        + lines[from..]
            .iter()
            .position(|l| l == "}")
            .expect("rollback_write never closes");
    let hold = (from..to)
        .find(|&i| is_code(&lines[i]) && lines[i].contains("place_hold \"forgejo-data-rollback-"))
        .expect("rollback_write places no hold");
    // Nothing before the hold mutates: the stop, the umount, the fstab
    // edit and the rename all come after it.
    for (i, l) in lines.iter().enumerate().take(hold).skip(from) {
        for m in ["stop -t", "umount", "mv ", "rmdir", "chattr", "STAGE="] {
            assert!(
                !(is_code(l) && l.contains(m)),
                "rollback_write mutates before its hold, line {}: {}",
                i + 1,
                l.trim()
            );
        }
    }
    no_refusal_after(&lines, hold, to, "rollback_write");
}

/// The restart's one mutation is the start: before it every failure is
/// a refusal, after it none is, and nothing in it stops Forgejo — the
/// verb exists because a stop left the forge down (backlog 85d29e33).
#[test]
fn the_restart_starts_and_never_stops() {
    let lines = code_lines(SCRIPT);
    let from = lines
        .iter()
        .position(|l| l.starts_with("restart_write() {"))
        .expect("restart_write is gone");
    let to = from
        + lines[from..]
            .iter()
            .position(|l| l == "}")
            .expect("restart_write never closes");
    let find = |needle: &str| {
        (from..to)
            .find(|&i| is_code(&lines[i]) && lines[i].contains(needle))
            .unwrap_or_else(|| panic!("restart_write has no code line with {needle:?}"))
    };
    let stage = find("STAGE=restarting");
    assert!(stage < find("start \"$SERVICE\""));
    no_refusal_after(&lines, stage, to, "restart_write");
    for l in lines[from..to].iter().filter(|l| is_code(l)) {
        for banned in ["stop -t", "proof_fail", "stop_then_fail", "real_start"] {
            assert!(!l.contains(banned), "restart_write can stop Forgejo: {l}");
        }
    }
}

/// The fstab is only ever REPLACED by a temporary file findmnt has
/// verified, and a mount is only ever of the one path: never `mount -a`,
/// never an append to the live file.
#[test]
fn the_fstab_is_edited_only_through_a_verified_file() {
    let lines = code_lines(SCRIPT);
    for l in lines.iter().filter(|l| is_code(l)) {
        assert!(!l.contains("mount -a"), "mount -a: {l}");
        assert!(
            !l.contains(">> \"$FSTAB\"") && !l.contains("> \"$FSTAB\""),
            "writes the live fstab: {l}"
        );
        assert!(!l.contains("|| true"), "a guard that cannot fail: {l}");
    }
    let replaces: Vec<usize> = (0..lines.len())
        .filter(|&i| is_code(&lines[i]) && lines[i].contains("mv -f -- \"$NEWTAB\" \"$FSTAB\""))
        .collect();
    assert_eq!(
        replaces.len(),
        2,
        "one replace for the move, one for the rollback"
    );
    for r in replaces {
        let verify = (0..r)
            .rev()
            .find(|&i| is_code(&lines[i]) && lines[i].contains("fstab_verifies \"$NEWTAB\""))
            .expect("a replace with no verify before it");
        let between_new_tab = (verify..r).any(|i| lines[i].contains("NEWTAB=$(mktemp"));
        assert!(
            !between_new_tab,
            "the verified file is not the one moved into place"
        );
    }
}

/// A read that fails is a refusal, never a value: no `|| echo 0`, and
/// every du and df answer is checked for digits before it is compared.
#[test]
fn a_read_that_fails_is_a_refusal_never_a_value() {
    let lines = code_lines(SCRIPT);
    for l in lines.iter().filter(|l| is_code(l)) {
        assert!(
            !l.contains("|| echo 0"),
            "a failed read recorded as a value: {l}"
        );
    }
    let body = lines.join("\n");
    assert!(body.contains("set -uo pipefail"));
    for n in [
        "[[ \"$alloc\" =~ ^[0-9]+$ ]]",
        "[[ \"$avail\" =~ ^[0-9]+$ ]]",
    ] {
        assert!(body.contains(n), "the script lacks the digit check {n}");
    }
}

// ---------------------------------------------------------------------------
// The verb files.
// ---------------------------------------------------------------------------

fn verb(name: &str) -> serde_json::Value {
    let p = repo_root().join(format!("infra/ops/verbs/{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).expect("a verb file is JSON")
}

#[test]
fn the_plan_verbs_are_read_only_and_the_writes_need_david_s_passkey() {
    for (write, plan, plan_flag) in [
        ("move-forgejo-data", "plan-a-forgejo-data-move", "--plan"),
        (
            "roll-back-forgejo-data-move",
            "plan-a-forgejo-data-rollback",
            "--plan-rollback",
        ),
        // The approved way back for a Forgejo a failed proof stopped
        // (backlog 85d29e33): the same plan/approve shape.
        (
            "restart-forgejo",
            "plan-a-forgejo-restart",
            "--plan-restart",
        ),
    ] {
        let w = verb(write);
        let p = verb(plan);
        assert_eq!(w["requires_approval"], true, "{write}");
        assert_eq!(w["plan_verb"], plan, "{write}");
        assert_eq!(w["approvers"], serde_json::json!(["emp-david"]), "{write}");
        assert_eq!(w["hosts"], serde_json::json!(["forge"]), "{write}");
        assert!(
            w["about"].as_str().unwrap().starts_with("MUTATING"),
            "{write}"
        );
        let params = w["params"].as_array().unwrap();
        assert_eq!(params.len(), 1, "{write} takes only the hash — no path");
        assert_eq!(params[0]["name"], "plan_sha256");
        assert_eq!(params[0]["pattern"], "^[0-9a-f]{64}$");
        let argv = w["argv"].as_array().unwrap();
        assert_eq!(argv[0], SCRIPT);
        assert_eq!(
            argv.last().unwrap(),
            "{1}",
            "{write}: the hash is the last word"
        );

        assert!(
            p.get("requires_approval").is_none(),
            "{plan} must need no approval"
        );
        assert!(
            p["about"].as_str().unwrap().starts_with("READ-ONLY"),
            "{plan}"
        );
        assert_eq!(p["params"], serde_json::json!([]), "{plan}");
        assert_eq!(p["argv"], serde_json::json!([SCRIPT, plan_flag]), "{plan}");
        assert_eq!(p["hosts"], w["hosts"], "{plan} renders where {write} runs");
    }
    assert_eq!(
        verb("roll-back-forgejo-data-move")["argv"],
        serde_json::json!([SCRIPT, "--rollback", "{1}"])
    );
    assert_eq!(
        verb("restart-forgejo")["argv"],
        serde_json::json!([SCRIPT, "--restart", "{1}"])
    );
    // A copy and a checksum pass over ~100 GB: the runner's 30 s default
    // would kill it mid-copy.
    assert!(verb("move-forgejo-data")["timeout"].as_u64().unwrap() >= 3600);
}
