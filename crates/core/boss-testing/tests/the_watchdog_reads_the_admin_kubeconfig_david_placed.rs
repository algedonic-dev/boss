//! The loops that ACT on the cluster when everything else is dark — the
//! forge watchdog (`infra/forge/cluster-watchdog.sh`) and the named
//! rollback (`infra/forge/rollback-to.sh`) — read the admin kubeconfig
//! David places and the converge checks, `${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig`,
//! through ONE helper in `infra/estate/ops-credentials.sh` (backlog
//! fb444bbb, car 1).
//!
//! WHY. Until this car the forge had two admin kubeconfigs: the one
//! install-cluster-operator checks (root:root 600 under /etc/boss-ops)
//! and `/home/david/kc.yaml`, which the watchdog's unit named and
//! nothing checked. A rotation of /etc/boss-ops left the last safety net
//! on the old credential, and the watchdog only reads the cluster in
//! earnest when it must roll back — the one moment a stale credential
//! costs the most.
//!
//! WHY `--mount` AND NO EXISTENCE TEST. The watchdog runs as david, and
//! /etc/boss-ops is a 0700 root directory: david cannot tell an absent
//! file from an unsearchable directory, so a `[ -f ]` of his would
//! refuse on every tick and switch the safety net off. The file is
//! reached only through `sudo docker run`, whose daemon mounts it as
//! root; `--mount type=bind,...` makes an absent source a refusal that
//! names the path, where `-v` would create it as a root-owned DIRECTORY
//! inside /etc/boss-ops. So docker's refusal is the signal, and the
//! watchdog must say it loudly rather than close "ok" because the API
//! answered.
//!
//! HOW THIS IS MEASURED. The watchdog and the read-only check run for
//! real with `sudo` and `curl` stubbed on PATH: the stub `sudo` records
//! its arguments and either answers an image or refuses exactly as
//! docker does for an absent bind source.
//!
//! NOT YET TREE-WIDE. The converge's two read-only checks
//! (check-manifests-applied.sh, the orphan lint through
//! undeclared-objects.sh) run a host `kubectl` as david and cannot read
//! a root:root 600 file, so they stay on the home copy until car 2 gives
//! them a scoped read-only credential. The pin below is therefore held
//! to the two acting loops, not to all of infra/.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LIB: &str = "infra/estate/ops-credentials.sh";

/// The acting loops this car moves, and the unit that configures one.
const ACTING: [&str; 3] = [
    "infra/forge/cluster-watchdog.sh",
    "infra/forge/cluster-watchdog.service",
    "infra/forge/rollback-to.sh",
];

fn sh(script: &str, ops_dir: Option<&str>) -> String {
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(format!(". '{}'\n{script}", repo_root().join(LIB).display()));
    c.env_remove("BOSS_OPS_DIR")
        .env_remove("BOSS_FORGE_OWNER")
        .env("BOSS_FORGE_REGISTRY_HOST", "reg.test");
    if let Some(d) = ops_dir {
        c.env("BOSS_OPS_DIR", d);
    }
    let out = c.output().expect("sh runs");
    assert!(out.status.success(), "{script}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_admin_kubeconfig_has_one_path_under_the_ops_dir() {
    assert_eq!(sh("ops_kubeconfig", None), "/etc/boss-ops/kubeconfig");
    assert_eq!(sh("ops_kubeconfig", Some("/x/ops")), "/x/ops/kubeconfig");
}

#[test]
fn the_kubectl_mounts_it_so_an_absent_file_refuses_by_name() {
    let k = sh("ops_kubectl", Some("/x/ops"));
    assert!(
        k.starts_with("sudo -n docker run ")
            && k.contains(" --mount type=bind,src=/x/ops/kubeconfig,dst=/kc,readonly ")
            && k.ends_with(" kubectl --kubeconfig=/kc"),
        "{k}"
    );
    assert!(
        !k.contains(" -v "),
        "-v creates an absent source as a root-owned directory: {k}"
    );
}

#[test]
fn the_acting_loops_read_only_the_one_path() {
    for f in ACTING {
        let text = std::fs::read_to_string(repo_root().join(f))
            .unwrap_or_else(|e| panic!("{f} is readable: {e}"));
        let code: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        for bad in [
            "kc.yaml",
            "KUBECONFIG_PATH",
            "BOSS_FORGE_KUBECONFIG",
            ":/kc:ro",
        ] {
            let hits: Vec<&&str> = code.iter().filter(|l| l.contains(bad)).collect();
            assert!(
                hits.is_empty(),
                "{f} names `{bad}` — the acting loops read the admin kubeconfig only \
                 through ops_kubectl (backlog fb444bbb): {hits:?}"
            );
        }
        if f.ends_with(".sh") {
            assert!(
                text.contains("estate/ops-credentials.sh") && text.contains("ops_kubectl"),
                "{f} must source the one helper and run its kubectl"
            );
            // david cannot search /etc/boss-ops: a test of his on the file
            // answers "absent" for a present credential.
            let probes: Vec<&&str> = code
                .iter()
                .filter(|l| {
                    ["-f ", "-e ", "-r ", "-s "].iter().any(|t| l.contains(t))
                        && (l.contains("ops_kubeconfig") || l.contains("$KC"))
                })
                .collect();
            assert!(
                probes.is_empty(),
                "{f} tests the kubeconfig's existence as the unit's user: {probes:?}"
            );
        }
    }
}

/// A fixture: stub `sudo` (records argv, then answers or refuses) and
/// `curl` (the API answers health; every other call fails, so an alert
/// is spooled, never posted anywhere).
struct Forge {
    dir: PathBuf,
    ops: PathBuf,
}

impl Forge {
    fn new(name: &str) -> Self {
        let dir = scratch_dir(name);
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        // `mute` fails with no output at all — a read that failed must
        // still read as blind, whatever docker did or did not say.
        // Docker's housekeeping calls first (ops_image_ready, review of
        // 8f50d314): the image is present unless STUB_IMAGE=absent and
        // nothing has pulled it; STUB_PULL=hang|fail plays a stalled or
        // refusing registry; the pin lives in a file beside the argv log,
        // and STUB_PIN=refuse makes `docker create` fail.
        write_exec(
            &bin.join("sudo"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$STUB_ARGV\"\n\
             d=$(dirname \"$STUB_ARGV\")\n\
             case \"$*\" in\n\
             '-n docker image inspect '*) [ \"${STUB_IMAGE:-present}\" = present ] || [ -e \"$d/pulled\" ]; exit $? ;;\n\
             '-n docker pull '*)\n\
             \x20 case \"${STUB_PULL:-ok}\" in\n\
             \x20 hang) exec sleep 30 ;;\n\
             \x20 fail) echo 'Error response from daemon: dial tcp: connect: connection refused' >&2; exit 1 ;;\n\
             \x20 esac\n\
             \x20 : > \"$d/pulled\"; exit 0 ;;\n\
             '-n docker container inspect '*) [ -e \"$d/pin\" ] || { echo 'Error: No such container' >&2; exit 1; }; cat \"$d/pin\"; exit 0 ;;\n\
             '-n docker create '*)\n\
             \x20 [ \"${STUB_PIN:-ok}\" = refuse ] && { echo 'Error response from daemon: no space left on device' >&2; exit 1; }\n\
             \x20 prev=''; img=''; for a in \"$@\"; do img=\"$prev\"; prev=\"$a\"; done\n\
             \x20 printf '%s\\n' \"$img\" > \"$d/pin\"; exit 0 ;;\n\
             '-n docker rm '*) rm -f \"$d/pin\"; exit 0 ;;\n\
             esac\n\
             src=''\nfor a in \"$@\"; do case \"$a\" in type=bind,src=*) src=\"${a#type=bind,src=}\"; src=\"${src%%,*}\";; esac; done\n\
             if [ \"$STUB_MODE\" = refuse ]; then\n\
             echo \"docker: Error response from daemon: invalid mount config for type \\\"bind\\\": bind source path does not exist: $src\" >&2\n\
             exit 125\nfi\n\
             if [ \"$STUB_MODE\" = mute ]; then exit 1; fi\n\
             echo 'registry.example/boss:abc1234'\n",
        );
        // STUB_API=down: the API is dark — health answers nothing.
        write_exec(
            &bin.join("curl"),
            "#!/bin/sh\ncase \"$*\" in *api/jobs/health*) [ \"${STUB_API:-up}\" = down ] && exit 7; echo '{\"commit\":\"abc1234def\"}'; exit 0;; esac\nexit 7\n",
        );
        write_file(&dir.join("stamp"), "abc1234\n");
        let ops = dir.join("boss-ops");
        Forge { dir, ops }
    }

    fn run(&self, script: &str, mode: &str, args: &[&str]) -> Output {
        self.run_with(script, mode, args, &[])
    }

    fn run_with(&self, script: &str, mode: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(script))
            .args(args)
            .env("PATH", path)
            .env("HOME", &self.dir)
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("JOBS_API", "http://127.0.0.1:1")
            .env("REGISTRY", "registry.example/boss")
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env_remove("BOSS_FORGE_OWNER")
            .env_remove("STUB_API")
            .env("BOSS_OPS_DIR", &self.ops)
            .env("BOSS_FORGE_LAST_BUILT", self.dir.join("stamp"))
            .env("WATCHDOG_STATE", self.dir.join("dark"))
            .env("WATCHDOG_BLIND_STATE", self.dir.join("blind"))
            .env("ALERT_SPOOL", self.dir.join("spool"))
            .env("BOSS_RUN_SUMMARY_FILE", self.dir.join("summary.json"))
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_MODE", mode);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("script runs")
    }

    fn kubeconfig(&self) -> String {
        self.ops.join("kubeconfig").display().to_string()
    }

    fn argv(&self) -> String {
        std::fs::read_to_string(self.dir.join("argv")).unwrap_or_default()
    }

    fn summary(&self) -> serde_json::Value {
        let body = std::fs::read_to_string(self.dir.join("summary.json")).expect("summary");
        serde_json::from_str(&body).expect("summary json")
    }

    fn spooled(&self) -> Vec<String> {
        let dir = self.dir.join("spool");
        if !Path::new(&dir).exists() {
            return Vec::new();
        }
        std::fs::read_dir(&dir)
            .expect("spool")
            .filter_map(Result::ok)
            .map(|e| std::fs::read_to_string(e.path()).expect("alert"))
            .collect()
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn the_watchdog_reads_the_cluster_through_the_ops_kubeconfig() {
    let f = Forge::new("watchdog-reads");
    let o = f.run("infra/forge/cluster-watchdog.sh", "answer", &[]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(0), "{t}");
    assert!(t.contains("deployment serves abc1234"), "{t}");
    assert!(
        f.argv().contains(&format!(
            "--mount type=bind,src={},dst=/kc,readonly",
            f.kubeconfig()
        )),
        "{}",
        f.argv()
    );
    let read = f.summary()["cluster_read"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        read.starts_with("ok: ") && read.contains(&f.kubeconfig()),
        "{read}"
    );
    assert!(
        f.spooled().is_empty(),
        "nothing to alert: {:?}",
        f.spooled()
    );
}

#[test]
fn a_watchdog_that_cannot_read_the_cluster_says_so_once_and_fails() {
    let f = Forge::new("watchdog-blind");
    let o = f.run("infra/forge/cluster-watchdog.sh", "refuse", &[]);
    let t = text(&o);
    // The API answered — that is NOT the same as the watchdog being able
    // to act, and the run must not close "ok".
    assert_ne!(o.status.code(), Some(0), "{t}");
    assert!(
        t.contains(&format!(
            "bind source path does not exist: {}",
            f.kubeconfig()
        )),
        "docker's own refusal, naming the path: {t}"
    );
    let read = f.summary()["cluster_read"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        read.starts_with("REFUSED: ") && read.contains(&f.kubeconfig()),
        "{read}"
    );
    let alerts = f.spooled();
    assert_eq!(alerts.len(), 1, "one alert on going blind: {t}");
    assert!(alerts[0].contains(&f.kubeconfig()), "{}", alerts[0]);

    // Still blind next tick: loud on the packet and in the journal, but
    // the urgent alert is not re-filed every five minutes.
    let again = f.run("infra/forge/cluster-watchdog.sh", "refuse", &[]);
    assert_ne!(again.status.code(), Some(0), "{}", text(&again));
    assert_eq!(f.spooled().len(), 1, "{}", text(&again));

    // Sight restored: the run is ok again and the next blindness alerts anew.
    let back = f.run("infra/forge/cluster-watchdog.sh", "answer", &[]);
    assert_eq!(back.status.code(), Some(0), "{}", text(&back));
    assert!(!f.dir.join("blind").exists(), "{}", text(&back));
}

#[test]
fn the_read_only_check_reads_the_deployment_through_the_same_path() {
    let f = Forge::new("kubeconfig-check");
    let o = f.run("infra/forge/admin-kubeconfig-reads.sh", "answer", &[]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(0), "{t}");
    assert!(
        t.contains(&format!(
            "admin-kubeconfig-reads: deploy/boss serves registry.example/boss:abc1234 through {}",
            f.kubeconfig()
        )),
        "{t}"
    );
    assert!(
        f.argv().contains(" get deploy boss -n boss "),
        "{}",
        f.argv()
    );
    assert!(!f.argv().contains(" patch "), "read-only: {}", f.argv());

    let r = f.run("infra/forge/admin-kubeconfig-reads.sh", "refuse", &[]);
    let t = text(&r);
    assert_eq!(r.status.code(), Some(1), "{t}");
    assert!(t.contains("REFUSED") && t.contains(&f.kubeconfig()), "{t}");
}

#[test]
fn rollback_refuses_before_acting_when_it_cannot_read_the_cluster() {
    let f = Forge::new("rollback-blind");
    let o = f.run("infra/forge/rollback-to.sh", "refuse", &["abc1234"]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("REFUSED") && t.contains(&f.kubeconfig()), "{t}");
    assert!(
        !f.argv().contains(" patch "),
        "it must not act blind: {}",
        f.argv()
    );
}

// --- An arm that needs the patient is not an arm (backlog cf321ffd) ----
//
// The RELEASE review of car fb444bbb found four ways the watchdog still
// owed something to the outage it exists for: blind AND dark it took the
// rollback branch and filed "rollback did not go Ready" every tick; its
// kubectl came from Docker Hub on a daemon the disk sweep prunes hourly;
// sudo without -n; and no bound on the read or the unit. Each is pinned
// below, and so is the other half: when it CAN read, it still rolls.

/// The one image both acting helpers run in: the forge registry's mirror
/// of alpine/k8s, spelled from the registry host the fixture names.
const MIRRORED: &str = "reg.test/david/alpine-k8s:1.33.3";

fn dark_past_the_limit() -> [(&'static str, &'static str); 2] {
    [("STUB_API", "down"), ("WATCHDOG_DARK_LIMIT", "1")]
}

#[test]
fn a_blind_watchdog_in_the_dark_asks_for_hands_with_the_blind_reason() {
    let f = Forge::new("watchdog-blind-dark");
    for tick in 1..=2 {
        let o = f.run_with(
            "infra/forge/cluster-watchdog.sh",
            "refuse",
            &[],
            &dark_past_the_limit(),
        );
        let t = text(&o);
        assert_eq!(o.status.code(), Some(1), "tick {tick}: {t}");
        assert!(
            !f.argv().contains(" patch "),
            "tick {tick}: a patch through a credential that cannot read cannot land: {}",
            f.argv()
        );
        assert!(!t.contains("rolling deploy/boss"), "tick {tick}: {t}");
    }
    let alerts = f.spooled();
    assert!(
        alerts
            .iter()
            .all(|a| !a.contains("did not go Ready") && !a.contains("rollback to")),
        "no alert may claim a rollback it never attempted: {alerts:#?}"
    );
    let hands: Vec<&String> = alerts
        .iter()
        .filter(|a| a.contains("hands needed"))
        .collect();
    assert_eq!(hands.len(), 2, "one hands alert per dark tick: {alerts:#?}");
    for a in &hands {
        assert!(
            a.contains("cannot read")
                && a.contains(&format!(
                    "bind source path does not exist: {}",
                    f.kubeconfig()
                )),
            "the hands alert carries the blind reason, not a guess: {a}"
        );
        assert!(
            !a.contains("last converged build itself"),
            "blind is not 'dark on the stamp' — it cannot tell what is served: {a}"
        );
    }
    assert_eq!(
        alerts
            .iter()
            .filter(|a| a.contains("watchdog blind"))
            .count(),
        1,
        "going blind is still said once: {alerts:#?}"
    );
}

#[test]
fn a_read_that_fails_without_a_word_is_still_blind() {
    let f = Forge::new("watchdog-mute");
    let o = f.run("infra/forge/cluster-watchdog.sh", "mute", &[]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    let read = f.summary()["cluster_read"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(read.starts_with("REFUSED: "), "{read}");
}

#[test]
fn a_watchdog_that_can_read_still_rolls_to_the_stamp_in_the_dark() {
    // The DR half: every change here must leave the lever working. The
    // cluster serves abc1234, the last converged build is def5678, the
    // API is dark past the limit, and the read works.
    let f = Forge::new("watchdog-rolls");
    write_file(&f.dir.join("stamp"), "def5678\n");
    let o = f.run_with(
        "infra/forge/cluster-watchdog.sh",
        "answer",
        &[],
        &dark_past_the_limit(),
    );
    let t = text(&o);
    assert_eq!(o.status.code(), Some(0), "{t}");
    let argv = f.argv();
    let patch = argv
        .lines()
        .find(|l| l.contains(" kubectl --kubeconfig=/kc patch deploy boss "))
        .unwrap_or_else(|| panic!("it rolled nothing: {argv}"));
    assert!(
        patch.starts_with("-n docker run ")
            && patch.contains(&format!(" {MIRRORED} "))
            && patch.contains("registry.example/boss:def5678")
            && patch.contains(" --request-timeout="),
        "the patch runs the mirrored kubectl under sudo -n, bounded, to the stamp: {patch}"
    );
    assert!(
        argv.lines()
            .any(|l| l.contains(" rollout status deploy/boss -n boss --timeout=")),
        "{argv}"
    );
    assert!(
        t.contains("cluster RESTORED on the last converged build def5678"),
        "{t}"
    );
    let alerts = f.spooled();
    assert_eq!(alerts.len(), 1, "{alerts:#?}");
    assert!(
        alerts[0].contains("cluster restored by the watchdog"),
        "{}",
        alerts[0]
    );
}

#[test]
fn kubectl_and_talosctl_run_one_mirrored_image_under_sudo_n() {
    assert_eq!(sh("ops_image", None), MIRRORED);
    for helper in ["ops_kubectl", "ops_talosctl"] {
        let line = sh(helper, Some("/x/ops"));
        assert!(line.starts_with("sudo -n docker run "), "{helper}: {line}");
        assert!(
            line.contains(&format!(" {MIRRORED} ")),
            "{helper} runs the one image: {line}"
        );
        assert!(
            !line.contains("alpine/k8s"),
            "{helper} pulls from Docker Hub, which the sweep's prune sends it back to every hour: {line}"
        );
    }
}

#[test]
fn the_ops_image_is_a_tag_the_mirror_puts_there() {
    // A fact that lives twice gets an equality test (CLAUDE.md §9a): the
    // mirror's list carries the tag, ops_image names it. Both are asked
    // for the same registry host, and the mirror derives its base the way
    // every forge verb does (forge-defaults.sh).
    let dir = scratch_dir("ops-image-mirrored");
    let env = dir.join("sor.env");
    write_file(&env, "BOSS_FORGE_REGISTRY_HOST=reg.test\n");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/mirror-base-images.sh"))
        .arg("--check")
        .env("BOSS_SOR_ENV", &env)
        .env_remove("BOSS_FORGE_REGISTRY_HOST")
        .env_remove("BOSS_FORGE_REGISTRY_BASE")
        .env_remove("BOSS_CI_REGISTRY")
        .env_remove("BOSS_FORGE_OWNER")
        .output()
        .expect("mirror --check runs");
    let listed = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{listed}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let image = sh("ops_image", None);
    assert!(
        listed
            .lines()
            .any(|l| l.trim_end().ends_with(&format!("->  {image}"))),
        "ops_image {image} is not a destination of mirror-base-images.sh — the watchdog \
         would pull a tag nothing puts there:\n{listed}"
    );
}

#[test]
fn with_no_registry_host_the_kubectl_refuses_by_name_and_pulls_nothing() {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            ". '{}'\nk=$(ops_kubectl)\necho \"[$k]\"\n$k get deploy boss",
            repo_root().join(LIB).display()
        ))
        .env_remove("BOSS_FORGE_REGISTRY_HOST")
        .output()
        .expect("sh runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_ne!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        !stdout.contains("docker"),
        "no registry host means no image: nothing may run docker, which would \
         read the next word as an image to pull: {stdout}"
    );
    assert!(stderr.contains("BOSS_FORGE_REGISTRY_HOST"), "{stderr}");
}

#[test]
fn every_read_on_the_acting_path_is_bounded_and_so_is_the_unit() {
    let f = Forge::new("watchdog-bounded");
    let o = f.run("infra/forge/cluster-watchdog.sh", "answer", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let argv = f.argv();
    let read = argv
        .lines()
        .find(|l| l.contains(" get deploy boss "))
        .unwrap_or_else(|| panic!("no read: {argv}"));
    assert!(
        read.contains(" --request-timeout="),
        "a half-dead API server holds an unbounded read, and the oneshot with it: {read}"
    );
    // rollback-to's first read is the same helper; its read-back carries
    // its own bound. Every `$K get` there says so.
    let rb = std::fs::read_to_string(repo_root().join("infra/forge/rollback-to.sh"))
        .expect("rollback-to");
    let unbounded: Vec<&str> = rb
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter(|l| l.contains("$K get ") && !l.contains("--request-timeout="))
        .collect();
    assert!(unbounded.is_empty(), "{unbounded:?}");

    let unit = std::fs::read_to_string(repo_root().join("infra/forge/cluster-watchdog.service"))
        .expect("the watchdog's unit");
    let bound = unit
        .lines()
        .find_map(|l| l.strip_prefix("TimeoutStartSec="))
        .unwrap_or_else(|| {
            panic!("the unit declares no TimeoutStartSec — a oneshot's default is infinity")
        });
    assert!(
        !bound.is_empty() && bound != "infinity" && bound != "0",
        "TimeoutStartSec={bound} does not bound the unit"
    );
}

#[test]
fn an_unreadable_dark_count_is_said_never_read_as_zero() {
    // backlog f280dd01 item 5: `cat "$STATE" || echo 0` read a count it
    // could not read as zero dark ticks. Absent is zero (the first dark
    // tick); unreadable or not a number is reported, and fails the run.
    for (name, plant) in [("dir", None), ("garbage", Some("three\n"))] {
        let f = Forge::new(&format!("watchdog-dark-{name}"));
        let state = f.dir.join("dark");
        match plant {
            None => std::fs::create_dir_all(&state).expect("a state path cat cannot read"),
            Some(body) => write_file(&state, body),
        }
        let o = f.run_with(
            "infra/forge/cluster-watchdog.sh",
            "answer",
            &[],
            &[("STUB_API", "down")],
        );
        let t = text(&o);
        assert_eq!(o.status.code(), Some(1), "{name}: {t}");
        assert!(
            t.contains("dark count") && t.contains(&state.display().to_string()),
            "{name}: the run names the state file it could not read: {t}"
        );
        let field = f.summary()["dark_count"].as_str().unwrap_or("").to_string();
        match plant {
            // A directory cannot be written back either, so every tick
            // would count from zero: that is the louder fact, and it wins.
            None => assert!(
                field.starts_with("UNWRITABLE: ") && field.contains("will NEVER roll"),
                "{name}: {field}"
            ),
            Some(_) => assert!(field.starts_with("UNREADABLE: "), "{name}: {field}"),
        }
    }
}

#[test]
fn a_dark_count_it_cannot_write_says_it_will_never_roll() {
    // Review of 8f50d314, L4: an unchecked write left the count at 1 on
    // every tick — a full root volume, and the watchdog never rolls.
    let f = Forge::new("watchdog-dark-unwritable");
    let state = f.dir.join("no-such-dir").join("dark");
    let state_s = state.display().to_string();
    let o = f.run_with(
        "infra/forge/cluster-watchdog.sh",
        "answer",
        &[],
        &[("STUB_API", "down"), ("WATCHDOG_STATE", &state_s)],
    );
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(
        t.contains("CANNOT COUNT, WILL NEVER ROLL") && t.contains(&state_s),
        "{t}"
    );
    let field = f.summary()["dark_count"].as_str().unwrap_or("").to_string();
    assert!(
        field.starts_with("UNWRITABLE: ") && field.contains("will NEVER roll"),
        "{field}"
    );
}

#[test]
fn the_mirror_image_is_pinned_so_the_sweeps_prune_cannot_take_it() {
    // Review of 8f50d314, L3: `image prune -af` never removes an image a
    // container references, stopped or not.
    let f = Forge::new("watchdog-pins");
    let o = f.run("infra/forge/cluster-watchdog.sh", "answer", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let want = format!("-n docker create --name boss-ops-image-pin {MIRRORED} true");
    assert!(f.argv().lines().any(|l| l == want), "{}", f.argv());
    let field = f.summary()["ops_image"].as_str().unwrap_or("").to_string();
    assert_eq!(
        field,
        format!("present {MIRRORED}, pinned by container boss-ops-image-pin")
    );
    // Pinned already: the next tick makes no second container.
    let again = f.run("infra/forge/cluster-watchdog.sh", "answer", &[]);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again));
    let creates = f
        .argv()
        .lines()
        .filter(|l| l.starts_with("-n docker create "))
        .count();
    assert_eq!(creates, 1, "{}", f.argv());
}

#[test]
fn a_pin_it_cannot_make_is_said_and_fails_the_run_but_the_read_stands() {
    let f = Forge::new("watchdog-unpinned");
    let o = f.run_with(
        "infra/forge/cluster-watchdog.sh",
        "answer",
        &[],
        &[("STUB_PIN", "refuse")],
    );
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    let s = f.summary();
    assert!(
        s["cluster_read"].as_str().unwrap_or("").starts_with("ok: "),
        "{s}"
    );
    let field = s["ops_image"].as_str().unwrap_or("").to_string();
    assert!(
        field.contains("NOT pinned") && field.contains("no space left on device"),
        "{field}"
    );
}

#[test]
fn a_pull_that_stalls_is_a_named_blind_reason_with_the_hand_fallback() {
    // Review of 8f50d314, M1: a registry that accepts and stalls used to
    // hold `docker run`'s own pull until systemd killed the unit, which
    // files nothing. Now it is bounded, and blind says why and what to do.
    let f = Forge::new("watchdog-pull-stalls");
    let started = std::time::Instant::now();
    let o = f.run_with(
        "infra/forge/cluster-watchdog.sh",
        "answer",
        &[],
        &[
            ("STUB_IMAGE", "absent"),
            ("STUB_PULL", "hang"),
            ("BOSS_OPS_PULL_TIMEOUT_S", "1"),
        ],
    );
    let t = text(&o);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the pull was not bounded: {t}"
    );
    assert_eq!(o.status.code(), Some(1), "{t}");
    let read = f.summary()["cluster_read"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let fallback = format!(
        "by hand: sudo docker pull docker.io/alpine/k8s:1.33.3 && sudo docker tag docker.io/alpine/k8s:1.33.3 {MIRRORED}"
    );
    assert!(
        read.starts_with("REFUSED: ")
            && read.contains("timed out after 1s")
            && read.contains(&fallback),
        "{read}"
    );
    assert!(
        !f.argv().lines().any(|l| l.starts_with("-n docker run ")),
        "no kubectl run behind a failed pull: {}",
        f.argv()
    );
    let alerts = f.spooled();
    assert!(
        alerts.iter().any(|a| a.contains("watchdog blind")
            && a.contains(&format!("tag docker.io/alpine/k8s:1.33.3 {MIRRORED}"))),
        "the blind alert names the hand fallback: {alerts:#?}"
    );
}

#[test]
fn the_hand_fallback_pulls_the_tag_the_mirror_copies() {
    // The fallback's upstream name lives beside the mirror list's; held
    // equal here (CLAUDE.md §9a).
    let dir = scratch_dir("ops-image-fallback");
    let env = dir.join("sor.env");
    write_file(&env, "BOSS_FORGE_REGISTRY_HOST=reg.test\n");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/mirror-base-images.sh"))
        .arg("--check")
        .env("BOSS_SOR_ENV", &env)
        .env_remove("BOSS_FORGE_REGISTRY_HOST")
        .env_remove("BOSS_FORGE_REGISTRY_BASE")
        .env_remove("BOSS_CI_REGISTRY")
        .env_remove("BOSS_FORGE_OWNER")
        .output()
        .expect("mirror --check runs");
    let listed = String::from_utf8_lossy(&out.stdout).to_string();
    let source = listed
        .lines()
        .find(|l| l.trim_end().ends_with(&format!("->  {MIRRORED}")))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_else(|| panic!("no mirror line for {MIRRORED}:\n{listed}"))
        .to_string();
    assert_eq!(
        sh("ops_image_fallback", None),
        format!("sudo docker pull {source} && sudo docker tag {source} {MIRRORED}")
    );
}

#[test]
fn the_units_start_budget_covers_every_bound_it_contains() {
    // Review of 8f50d314, M1: 10min left out the wrap's retry window and
    // raced the failing rollback's own alert. Every term is read from the
    // file that sets it, so a bound raised there reds this, not the host.
    let read = |p: &str| {
        std::fs::read_to_string(repo_root().join(p)).unwrap_or_else(|e| panic!("{p}: {e}"))
    };
    let num_after = |text: &str, marker: &str| -> u64 {
        let at = text
            .find(marker)
            .unwrap_or_else(|| panic!("`{marker}` not found"))
            + marker.len();
        text[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or_else(|e| panic!("`{marker}`: {e}"))
    };
    let unit = read("infra/forge/cluster-watchdog.service");
    let wd = read("infra/forge/cluster-watchdog.sh");
    let ops = read("infra/estate/ops-credentials.sh");
    let curl = read("infra/boss-api-curl.sh");
    assert!(
        unit.contains("\nExecStartPre=-/var/lib/boss/tree/current/infra/boss-maintenance-wrap.sh "),
        "the budget below counts the wrap; if it went, recount"
    );
    let wrap = num_after(&curl, "${BOSS_API_RETRY_DEADLINE:-");
    let health = num_after(&wd, "curl -s --max-time ");
    let pull = num_after(&ops, "${BOSS_OPS_PULL_TIMEOUT_S:-");
    let read_t = num_after(&ops, "get deploy boss -n boss --request-timeout=");
    let patch = num_after(&wd, "patch deploy boss -n boss --request-timeout=");
    let rollout = num_after(&wd, "rollout status deploy/boss -n boss --timeout=");
    let door = 60; // observe-door.sh: a handful of 5 s probes and one post
    let need = wrap + health + pull + read_t + patch + rollout + door;
    let minutes = num_after(&unit, "\nTimeoutStartSec=");
    assert!(
        unit.contains(&format!("\nTimeoutStartSec={minutes}min\n")),
        "TimeoutStartSec is read in minutes"
    );
    assert!(
        minutes * 60 >= need + 60,
        "TimeoutStartSec={minutes}min is {}s; the start phase can take {need}s \
         (wrap {wrap} + health {health} + pull {pull} + read {read_t} + patch {patch} \
         + rollout {rollout} + door {door}) and the failing rollback's alert needs room",
        minutes * 60
    );
}

/// THE STAMP IS ANOTHER ACCOUNT'S FILE, READ AS DATA (backlog a604a35b).
/// The watchdog runs as root; the stamp is written by the cluster
/// converge as the checkout's owner, who can repoint the name at
/// anything root can read. In the dark, past the limit, with the read
/// working: a stamp that is not a build name rolls NOTHING, the run asks
/// for hands, and the bytes found reach neither the journal, the patch,
/// the packet nor the alert that is kept.
#[test]
fn a_stamp_that_is_not_a_build_name_rolls_nothing_and_is_never_printed() {
    for (tag, body) in [
        ("words", "SECRET-LOOKING-BYTES-not-a-build\n"),
        ("long-hex", "0123456789abcdef0123456789abcdef01234567\n"),
        ("injection", "abc1234\",\"x\":\"y\n"),
    ] {
        let f = Forge::new(&format!("watchdog-stamp-{tag}"));
        write_file(&f.dir.join("stamp"), body);
        let o = f.run_with(
            "infra/forge/cluster-watchdog.sh",
            "answer",
            &[],
            &dark_past_the_limit(),
        );
        let t = text(&o);
        assert_eq!(
            o.status.code(),
            Some(1),
            "{tag}: hands needed is a failed run: {t}"
        );
        let argv = f.argv();
        assert!(
            !argv.contains(" patch deploy boss "),
            "{tag}: nothing may be rolled to a stamp that is not a build name: {argv}"
        );
        let first = body.lines().next().unwrap();
        let alerts = f.spooled().join("\n");
        for (place, said) in [
            ("the journal", &t),
            ("the kubectl argv", &argv),
            ("the kept alert", &alerts),
        ] {
            assert!(
                !said.contains(first),
                "{tag}: the stamp's bytes reached {place}: {said}"
            );
        }
        assert!(t.contains("does not hold a build name"), "{tag}: {t}");
        assert!(
            f.summary()["stamp"]
                .as_str()
                .unwrap_or("")
                .starts_with("UNUSABLE"),
            "{tag}: {}",
            f.summary()
        );
    }
}

/// THE STAMP'S PATH IS NOT OPENED UNLESS IT IS A FILE OF ITS OWN (review
/// 5f3736a2, F6). The name is another account's to make: a symlink made
/// root open whatever it pointed at — a device node, a secret — and a
/// FIFO cost each tick its read bound. A symlink to a file holding a
/// perfectly good build name is the sharpest case, because the shape
/// check passes it: it must roll nothing all the same. So must a
/// directory standing at the name.
#[test]
fn a_stamp_that_is_not_a_regular_file_of_its_own_is_never_opened() {
    let f = Forge::new("watchdog-stamp-symlink");
    std::fs::remove_file(f.dir.join("stamp")).unwrap();
    write_file(&f.dir.join("elsewhere"), "def5678\n");
    std::os::unix::fs::symlink(f.dir.join("elsewhere"), f.dir.join("stamp")).unwrap();
    let o = f.run_with(
        "infra/forge/cluster-watchdog.sh",
        "answer",
        &[],
        &dark_past_the_limit(),
    );
    let t = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "a symlinked stamp is no stamp — hands: {t}"
    );
    assert!(
        !f.argv().contains(" patch deploy boss "),
        "the watchdog rolled to what a symlink pointed at: {}",
        f.argv()
    );
    assert!(!t.contains("def5678"), "{t}");
    assert!(t.contains("is not a regular file of its own"), "{t}");

    let d = Forge::new("watchdog-stamp-directory");
    std::fs::remove_file(d.dir.join("stamp")).unwrap();
    std::fs::create_dir(d.dir.join("stamp")).unwrap();
    let o = d.run_with(
        "infra/forge/cluster-watchdog.sh",
        "answer",
        &[],
        &dark_past_the_limit(),
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains("is not a regular file of its own"),
        "{}",
        text(&o)
    );
}
