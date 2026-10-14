//! The cluster converge runs as ROOT, from ROOT'S TREE, on the SYSTEM
//! docker daemon, and pushes with the credential broker's registry token
//! (backlog 683d7fe9, car 2; decided by David 2026-10-07 on design-doc
//! c8502e17, question image-build, and as position B of the forge sudo
//! decision). The runner and converge-now.sh are RUN here — against a
//! scratch forge repository, a scratch root tree (infra/forge/root-tree.sh,
//! the real one), stub `docker`, `curl` and `systemctl` — so each property
//! below is one they actually have.
//!
//! WHAT CHANGED, AND WHAT EACH CASE PINS:
//!
//!   * The runner fetched and checked out david's clone. It now brings
//!     root's tree to the forge's main (`root-tree.sh refresh`), and runs
//!     — and builds — from a hard-linked VIEW of that generation under
//!     its own state directory, so a converge that outlives two merges
//!     cannot have its tree pruned under it, and a change to the runner
//!     runs on its own train's converge.
//!   * Its stamps, its request inbox and its tenant checkouts lived in
//!     david's home and .git; they live under CONVERGE_STATE
//!     (forge-defaults.sh), root's, and the first root run carries the
//!     stamps across ONCE — reading them as data, a build name or
//!     nothing — because a converge that lost its stamp would apply the
//!     manifest's placeholder tag (the 2026-09-05 brick).
//!   * Its kubectl went through `sudo docker` with ~/kc.yaml; it is
//!     `docker` as root with the kubeconfig David places.
//!   * It pushed with the login the operator once typed; it refuses to
//!     build at all until the broker's login has been deposited, and
//!     presents that login to the registry and nothing else.
//!   * A run that is NOT root at the host's default tree is the unit
//!     installed before this car: it refuses and says so.

use boss_testing::{copy_exec, create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const SWEEP: &str = "infra/forge/disk-floor-sweep.sh";
const MIRROR: &str = "infra/forge/mirror-base-images.sh";
const REQUEST: &str = "15bd2027-0000-4000-8000-000000000000";

fn text(out: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The lines a shell runs: comments dropped.
fn live(src: &str) -> Vec<&str> {
    src.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A scratch forge: the product's repository holding this tree's copies
/// of everything the runner reads on its cheapest paths, a root tree to
/// refresh from it, a converge state directory, an old home holding the
/// stamps the converge kept before it moved, and stub tools.
struct Forge {
    root: PathBuf,
    bare: PathBuf,
    head: String,
    bin: PathBuf,
    log: PathBuf,
}

impl Forge {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("converge-root-tree-{name}"));
        let work = root.join("work");
        create_dir(&work);
        let tree = repo_root();
        // What the runner sources and runs before it builds, and what
        // root-tree.sh asks a generation to carry (the converge and the
        // installer).
        for f in [
            "infra/forge/cluster-deploy-lib.sh",
            "infra/forge/cluster-deploy-runner.sh",
            "infra/forge/forge-defaults.sh",
            "infra/forge/forge-repo-path.sh",
            "infra/forge/root-tree.sh",
            "infra/forge/alert-lib.sh",
            "infra/forge/forge-converge.sh",
            "infra/forge/install.sh",
            "infra/estate/ops-credentials.sh",
            "infra/run-summary.sh",
            "infra/cluster/render-instance.sh",
            "infra/cluster/render-tunnel-config.sh",
            "infra/cluster/instances-skipped.lib.sh",
            "infra/cluster/instances.toml",
            "infra/cluster/instance-manifests.txt",
        ] {
            let to = work.join(f);
            create_dir(to.parent().unwrap());
            copy_exec(&tree.join(f), &to);
        }
        // Whole directories, never a hand-kept list of their contents
        // (CLAUDE.md 9a): the libs every script sources, and the
        // manifests the renderer reads.
        for (dir, ext) in [
            ("infra/lib", None),
            ("infra/cluster/manifests", Some("yaml")),
        ] {
            create_dir(&work.join(dir));
            for e in std::fs::read_dir(tree.join(dir)).unwrap() {
                let p = e.unwrap().path();
                if p.is_file() && ext.is_none_or(|x| p.extension().is_some_and(|e| e == x)) {
                    copy_exec(&p, &work.join(dir).join(p.file_name().unwrap()));
                }
            }
        }
        // `tenant_dir` names a directory the renderer checks for a
        // manifest (f4f5c387).
        for tenant in read("infra/cluster/instances.toml")
            .lines()
            .filter_map(|l| l.trim().strip_prefix("tenant_dir = \""))
            .filter_map(|l| l.strip_suffix('"'))
        {
            let p = work.join(tenant).join("seeds/tenant.toml");
            create_dir(p.parent().unwrap());
            write_file(&p, "");
        }
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "the tree"]);
        let head = git(&work, &["rev-parse", "HEAD"]);
        // The forge's own repository, which root's tree fetches from.
        let bare = root.join("forge/david/boss.git");
        create_dir(bare.parent().unwrap());
        git(
            &root,
            &[
                "clone",
                "-q",
                "--bare",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        create_dir(&root.join("converge"));
        create_dir(&root.join("oldhome"));
        let bin = root.join("bin");
        create_dir(&bin);
        let log = root.join("calls");
        // curl's body rides stdin (`--data-binary @-`): logged beside the
        // arguments, answered 204 the way the API does.
        write_exec(
            &bin.join("curl"),
            "#!/usr/bin/env bash\necho \"curl $* body=$(cat)\" >> \"$STUB_LOG\"\nprintf 204\n",
        );
        // docker: every call logged with the DOCKER_CONFIG it ran under;
        // the observation's reads answered (a client dry run with
        // nothing, `get secret` not found, `rollout status` rolled out);
        // a build fails, so a deploying tick stops after it.
        write_exec(
            &bin.join("docker"),
            "#!/usr/bin/env bash\necho \"docker[${DOCKER_CONFIG:-}] $*\" >> \"$STUB_LOG\"\n\
             case \"$*\" in\n  *\" get secret \"*) echo 'Error from server (NotFound): secrets not found' >&2; exit 1 ;;\n\
             build*) echo 'error: could not compile `boss`' ; exit 101 ;;\nesac\nexit 0\n",
        );
        write_exec(
            &bin.join("sudo"),
            "#!/usr/bin/env bash\necho \"sudo $*\" >> \"$STUB_LOG\"\nexit 1\n",
        );
        Self {
            root,
            bare,
            head,
            bin,
            log,
        }
    }

    fn head7(&self) -> &str {
        &self.head[..7]
    }

    fn state(&self) -> PathBuf {
        self.root.join("converge")
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn summary(&self) -> serde_json::Value {
        std::fs::read_to_string(self.root.join("summary.json"))
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap_or(serde_json::Value::Null)
    }

    /// A second and third commit on the forge's main: a manifest added,
    /// then deleted — the history the converge's orphan check walks
    /// (infra/lint/a-deleted-manifest-leaves-no-object.sh, property A).
    /// The head moves to the deletion.
    fn delete_a_manifest_on_main(&mut self) {
        let work = self.root.join("work-history");
        git(
            &self.root,
            &[
                "clone",
                "-q",
                self.bare.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        write_file(
            &work.join("infra/cluster/manifests/gone.yaml"),
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: gone\n  namespace: boss\n",
        );
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "declare a service"]);
        git(&work, &["rm", "-q", "infra/cluster/manifests/gone.yaml"]);
        git(&work, &["commit", "-q", "-m", "delete its manifest"]);
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
        self.head = git(&work, &["rev-parse", "HEAD"]);
    }

    /// The view this tick ran from: the one `run-tree.<pid>` under the
    /// state directory that the case did not plant.
    fn views(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(self.state())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("run-tree")
            })
            .collect();
        v.sort();
        v
    }

    /// The runner, started from THIS tree's copy (as the unit starts the
    /// generation `current` named when it began), with every seam aimed
    /// at the scratch forge.
    fn run(&self, extra: &[(&str, &str)]) -> Output {
        self.run_without(extra, &[])
    }

    /// The same, with each of `absent` removed from the environment after
    /// the seams are set — a run as a person would start it by hand.
    fn run_without(&self, extra: &[(&str, &str)], absent: &[&str]) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(RUNNER))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", self.root.join("home-is-not-read"))
            .env("STUB_LOG", &self.log)
            .env("BOSS_ROOT_TREE", self.root.join("tree"))
            .env("BOSS_ROOT_TREE_SOURCE", &self.bare)
            .env("ROOT_TREE_SOURCE_RUNAS", "")
            .env("BOSS_FORGE_CONVERGE_STATE", self.state())
            .env("BOSS_FORGE_LEGACY_STATE_HOME", self.root.join("oldhome"))
            .env("BOSS_FORGE_KUBECONFIG", self.root.join("kubeconfig"))
            .env("BOSS_REGISTRY_PUSH_DIR", self.root.join("push"))
            .env("BOSS_CONVERGE_HOLD", self.root.join("hold"))
            .env("BOSS_FORGE_REGISTRY", "reg.test/david/boss")
            .env("BOSS_JOBS_URL", "http://stub:7900")
            .env("BOSS_SOR_ENV", self.root.join("no-sor.env"))
            .env("BOSS_MACHINE_TOKEN_DIR", self.root.join("no-machine-token"))
            .env("BOSS_RUN_SUMMARY_FILE", self.root.join("summary.json"))
            .env("ALERT_SPOOL", self.root.join("alert-spool"))
            .env("DOCKER_HOST", "unix:///run/user/1000/docker.sock")
            .env_remove("BOSS_RUNNER_VIEW");
        write_file(&self.root.join("kubeconfig"), "apiVersion: v1\n");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        for k in absent {
            cmd.env_remove(k);
        }
        cmd.output().expect("the runner runs")
    }
}

/// The cheapest path end to end: main unchanged since the stamp the
/// converge kept in its OLD home. The run refreshes root's tree from the
/// forge's repository, hands over to a view of that generation, carries
/// the stamp across, finds main unchanged, observes the connector through
/// `docker` as root with the placed kubeconfig — never sudo, never the
/// rootless socket the environment carried — and on its way out answers
/// the request that started it.
#[test]
fn the_unchanged_tick_runs_from_a_view_of_roots_tree_and_answers_its_request() {
    let f = Forge::new("unchanged");
    write_file(
        &f.root.join("oldhome/.boss-last-built"),
        &format!("{}\n", f.head7()),
    );
    write_file(&f.state().join("requests"), &format!("{REQUEST}\n"));
    let out = f.run(&[]);
    let t = text(&out);
    assert!(
        out.status.success(),
        "main unchanged is a clean exit 0: {t}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains(&format!("forge main unchanged ({})", f.head7())),
        "{t}"
    );
    // Root's tree was brought to the forge's main, and the run executed
    // from a view of exactly that generation.
    let tree = f.root.join("tree");
    let current = std::fs::read_link(tree.join("current")).expect("current names a generation");
    assert_eq!(current, PathBuf::from(format!("gen/{}", f.head)));
    // Root's tree is refreshed ONCE per tick, by the stage that hands
    // over; the view's own stage does not refresh it again (review
    // 1ba2873d, N3).
    assert_eq!(
        String::from_utf8_lossy(&out.stdout)
            .matches("cluster-deploy-runner: root-tree: ")
            .count(),
        1,
        "one refresh per tick: {t}"
    );
    let views = f.views();
    assert_eq!(views.len(), 1, "one view, this run's: {views:?}");
    let view = views[0].clone();
    assert_eq!(
        std::fs::read_to_string(view.join(".boss-tree-sha"))
            .unwrap()
            .trim(),
        f.head,
        "the view is the generation that is main"
    );
    assert!(
        view.join("infra/forge/cluster-deploy-runner.sh").is_file(),
        "the run handed over to the view's own runner"
    );
    // The packet says which generation ran, and as whom.
    assert!(
        f.summary()["runs_from"]
            .as_str()
            .is_some_and(|s| s.starts_with(&format!("root's tree {} as ", f.head))),
        "{}",
        f.summary()
    );
    // The stamp came across once, and the record says so.
    assert_eq!(
        std::fs::read_to_string(f.state().join("last-built"))
            .unwrap()
            .trim(),
        f.head7()
    );
    assert!(f.state().join(".carried").is_file());
    assert!(
        f.summary()["converge_state"]
            .as_str()
            .is_some_and(|s| s.contains("last-built")),
        "{}",
        f.summary()
    );
    // The connector was observed through root's docker and the placed
    // kubeconfig; nothing went through sudo.
    let calls = f.calls();
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("cloudflared: connected (observed on unchanged)"),
        "{t}"
    );
    assert!(
        calls.contains(&format!(
            "docker[] run --rm --log-driver=none --network host -v {}:/kc:ro",
            f.root.join("kubeconfig").display()
        )) && calls.contains("kubectl --kubeconfig=/kc rollout status deploy/cloudflared -n boss"),
        "the connector read went through the runner's own kubectl, as root: {calls}"
    );
    assert!(
        !calls.contains("sudo "),
        "nothing goes through sudo: {calls}"
    );
    // The request was answered and forgotten.
    assert!(
        calls.contains("-X PATCH") && calls.contains(&format!("/api/jobs/{REQUEST}/metadata")),
        "the run's exit answers the request that started it: {calls}\n{t}"
    );
    assert!(
        calls.contains(&format!(r#"{{"converged":"{} (unchanged)"}}"#, f.head7())),
        "{calls}"
    );
    assert!(
        !f.state().join("requests").exists() && !f.state().join("requests.run").exists(),
        "the request is answered once"
    );
    // A second tick carries nothing again: the old home is not read.
    write_file(&f.root.join("oldhome/.boss-last-built"), "0000000\n");
    let out = f.run(&[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        std::fs::read_to_string(f.state().join("last-built"))
            .unwrap()
            .trim(),
        f.head7(),
        "the carry runs once"
    );
}

/// A DEPLOYING tick refuses to build until the broker's push login has
/// been deposited — an hour into a cold build is the wrong moment to
/// learn it is not there — and with the login in place it builds the
/// generation it runs from, attested by the full sha the generation
/// names, and keeps its build-failure stamp in its own state directory.
#[test]
fn a_deploying_tick_builds_nothing_without_the_deposited_push_login() {
    let f = Forge::new("no-login");
    write_file(&f.root.join("oldhome/.boss-last-built"), "0000000\n");
    write_file(&f.state().join("requests"), &format!("{REQUEST}\n"));
    let out = f.run(&[]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no registry push login at"),
        "{t}"
    );
    let calls = f.calls();
    assert!(!calls.contains(" build "), "nothing was built: {calls}");
    assert!(!calls.contains(" push "), "nothing was pushed: {calls}");
    assert_eq!(
        f.summary()["failed_stage"],
        "registry login",
        "{}",
        f.summary()
    );
    assert!(
        calls.contains(r#"{"converge_failed":"registry login (exit 1)"}"#),
        "the request that asked reads why: {calls}"
    );

    // The login deposited: the build runs, from the view, tagged with the
    // seven-character name and attested with the generation's full sha.
    create_dir(&f.root.join("push"));
    write_file(&f.root.join("push/config.json"), "{\"auths\":{}}\n");
    let out = f.run(&[]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(101), "the stub build fails: {t}");
    let calls = f.calls();
    assert!(
        calls.contains(&format!(
            "build -f infra/oss-quickstart/Dockerfile --build-arg BOSS_BUILD_COMMIT={} -t reg.test/david/boss:{} .",
            f.head,
            f.head7()
        )),
        "{calls}"
    );
    assert_eq!(
        std::fs::read_to_string(f.state().join("last-build-failed"))
            .unwrap()
            .trim(),
        f.head7(),
        "the build-failure stamp is the converge's own"
    );
    assert!(
        !calls.contains(" push "),
        "a failed build pushes nothing: {calls}"
    );
}

/// A REFRESH THAT FAILS FAILS THE RUN: a forge repository root's tree
/// cannot fetch from builds nothing, and the stage says it.
#[test]
fn a_tree_that_cannot_be_refreshed_builds_nothing() {
    let f = Forge::new("no-source");
    write_file(
        &f.root.join("oldhome/.boss-last-built"),
        &format!("{}\n", f.head7()),
    );
    let out = f.run(&[(
        "BOSS_ROOT_TREE_SOURCE",
        f.root.join("no-such-repository.git").to_str().unwrap(),
    )]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(
        t.contains("root's tree could not be brought to the forge's main"),
        "{t}"
    );
    assert_eq!(f.summary()["failed_stage"], "fetch", "{}", f.summary());
    assert!(!f.calls().contains("docker"), "nothing ran: {}", f.calls());
}

/// THE CARRY READS ANOTHER ACCOUNT'S FILE AS DATA: a stamp that is not a
/// build name is not carried and not printed, and a link is not opened.
#[test]
fn the_carry_takes_only_a_build_name_from_the_old_home() {
    let f = Forge::new("carry");
    write_file(
        &f.root.join("oldhome/.boss-last-built"),
        "SECRET-ish content that is no build name\n",
    );
    write_file(&f.root.join("target-of-link"), "abcdef0\n");
    std::os::unix::fs::symlink(
        f.root.join("target-of-link"),
        f.root.join("oldhome/.boss-last-failed"),
    )
    .unwrap();
    let script = format!(
        "set -euo pipefail\n. '{}'\nrun_summary_field() {{ :; }}\nCONVERGE_STATE='{}'\ncarry_converge_state\n",
        repo_root().join(LIB).display(),
        f.state().display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("BOSS_FORGE_LEGACY_STATE_HOME", f.root.join("oldhome"))
        .output()
        .unwrap();
    let t = text(&out);
    assert!(out.status.success(), "{t}");
    assert!(!f.state().join("last-built").exists(), "{t}");
    assert!(
        !f.state().join("last-failed").exists(),
        "a link is not followed: {t}"
    );
    assert!(
        !t.contains("SECRET-ish"),
        "what it found is never printed: {t}"
    );
    assert!(t.contains("does not hold a build name"), "{t}");
    assert!(f.state().join(".carried").is_file(), "the carry ran, once");
}

/// A run that is not root, at the host's own tree, is the unit installed
/// before this car: it refuses before it reads or writes anything. Run
/// only where the test itself is not root (the gate runs as uid 65534);
/// as root the same line is the one the converge passes.
#[test]
fn a_run_that_is_not_root_at_the_hosts_tree_refuses() {
    let src = read(RUNNER);
    let code = live(&src);
    let refuse = code
        .iter()
        .position(|l| l.contains("REFUSED — this converge runs as root from root's tree"))
        .expect("the runner refuses a run that is not root");
    let first_write = code
        .iter()
        .position(|l| l.contains("run_summary_reset") || l.contains("root-tree.sh\" refresh"))
        .expect("the runner resets its summary and refreshes the tree");
    assert!(
        refuse < first_write,
        "the refusal comes before anything is read or written"
    );
    let uid = Command::new("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&uid.stdout).trim() == "0" {
        eprintln!("NOT RUN as root here: the refusal is for a run that is not root");
        return;
    }
    let out = Command::new("bash")
        .arg(repo_root().join(RUNNER))
        .env_remove("BOSS_ROOT_TREE")
        .env(
            "BOSS_RUN_SUMMARY_FILE",
            scratch_dir("converge-not-root").join("s.json"),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("predates that car"),
        "{}",
        text(&out)
    );
}

/// `converge-now.sh` (the `converge` ops verb) notes the ops-request
/// that started the runner in the converge's own state directory; the
/// runner takes the note and, when it ends, PATCHes each request with
/// its outcome — so a converge that died does not read as a clean
/// `answered` (backlog d66f92b2). The inbox was the checkout owner's
/// .git until the converge moved to root.
#[test]
fn a_converge_started_by_a_request_reports_its_outcome_on_that_request() {
    let root = scratch_dir("converge-root-tree-request");
    let state = root.join("converge");
    let bin = root.join("bin");
    create_dir(&bin);
    let log = root.join("calls");
    write_exec(
        &bin.join("systemctl"),
        "#!/usr/bin/env bash\necho \"systemctl $*\" >> \"$STUB_LOG\"\n\
         case \"$1\" in show) printf 'InvocationID=%032d\\nActiveState=activating\\nResult=success\\n' \
         \"$(grep -c 'systemctl start' \"$STUB_LOG\")\" ;; esac\n",
    );
    write_exec(
        &bin.join("curl"),
        "#!/usr/bin/env bash\necho \"curl $* body=$(cat)\" >> \"$STUB_LOG\"\nprintf 204\n",
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let id = "19df6925-0000-4000-8000-000000000000";
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/converge-now.sh"))
        .env("PATH", &path)
        .env("STUB_LOG", &log)
        .env("BOSS_FORGE_CONVERGE_STATE", &state)
        .env("BOSS_SOR_ENV", root.join("no-sor.env"))
        .env("OPS_REQUEST_ID", id)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("systemctl start --no-block cluster-deploy-runner.service"),
        "the verb still starts the unit"
    );
    let inbox = state.join("requests");
    assert_eq!(std::fs::read_to_string(&inbox).unwrap().trim(), id);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700,
        "the state directory is made as the unit declares it: root's alone"
    );

    // The runner's side, from cluster-deploy-lib.sh: take, then answer.
    let lib = |body: &str| {
        Command::new("bash")
            .arg("-c")
            .arg(format!(". '{}'; {body}", repo_root().join(LIB).display()))
            .arg("_")
            .arg(&state)
            .env("PATH", &path)
            .env("STUB_LOG", &log)
            .env("BOSS_JOBS_URL", "http://stub:7900")
            .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-machine-token"))
            .output()
            .unwrap()
    };
    let out = lib(
        "take_converge_requests \"$1\"; answer_converge_requests \"$1\" converge_failed 'build (exit 1)'",
    );
    assert!(out.status.success(), "{}", text(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("-X PATCH")
            && calls.contains(&format!("http://stub:7900/api/jobs/{id}/metadata"))
            && calls.contains(r#"{"converge_failed":"build (exit 1)"}"#),
        "the outcome is a metadata PATCH on the request that asked: {calls}"
    );
    assert!(!inbox.exists() && !state.join("requests.run").exists());

    // A stray line that is not a job id never reaches a URL.
    write_file(&inbox, "../../etc\n");
    let _ = std::fs::remove_file(&log);
    let out =
        lib("take_converge_requests \"$1\"; answer_converge_requests \"$1\" converged abc1234");
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("curl"),
        "a malformed id must not become a request"
    );
}

/// Every command that talks to the registry presents the DEPOSITED login
/// — the push, the converge's and the sweep's tag verification, the
/// mirror — and nothing unattended reaches the operator's rootless
/// daemon or his docker config, or goes through sudo, any more.
#[test]
fn the_registry_is_spoken_to_with_the_deposited_login_and_no_rootless_daemon_is_used() {
    let runner = read(RUNNER);
    let sweep = read(SWEEP);
    let mirror = read(MIRROR);
    for (file, src, want) in [
        (
            RUNNER,
            &runner,
            "DOCKER_CONFIG=\"$REGISTRY_PUSH_DIR\" docker push \"$REGISTRY:$HEAD\"",
        ),
        (
            RUNNER,
            &runner,
            "DOCKER_CONFIG=\"$REGISTRY_PUSH_DIR\" prune_registry_verified_tags \"$REGISTRY\"",
        ),
        (
            SWEEP,
            &sweep,
            "DOCKER_CONFIG=\"$REGISTRY_PUSH_DIR\" prune_registry_verified_tags \"$REGISTRY\"",
        ),
        (
            MIRROR,
            &mirror,
            "export DOCKER_CONFIG=\"${BOSS_MIRROR_DOCKER_CONFIG:-$REGISTRY_PUSH_DIR}\"",
        ),
        (RUNNER, &runner, "unset DOCKER_HOST"),
        (SWEEP, &sweep, "unset DOCKER_HOST"),
    ] {
        assert!(
            live(src).iter().any(|l| l.contains(want)),
            "{file} lost `{want}`"
        );
    }
    for (file, src) in [(RUNNER, &runner), (SWEEP, &sweep), (MIRROR, &mirror)] {
        for l in live(src) {
            for gone in ["run/user/1000", "/home/david", "sudo ", "$HOME", "kc.yaml"] {
                assert!(
                    !l.contains(gone),
                    "{file} still reaches `{gone}` on a line it runs: {l}"
                );
            }
        }
    }
    let defaults = read("infra/forge/forge-defaults.sh");
    assert!(
        defaults
            .contains("REGISTRY_PUSH_DIR=\"${BOSS_REGISTRY_PUSH_DIR:-/etc/boss/registry-push}\""),
        "the push login's directory is spelled once, in forge-defaults.sh"
    );
    let converge = read("infra/forge/forge-converge.sh");
    assert!(
        converge.contains("--dest \"${BOSS_REGISTRY_PUSH_DIR:-/etc/boss/registry-push}\""),
        "the deposit writes where forge-defaults.sh says the login is"
    );
}

/// Both units run as root from root's tree, and keep what they keep
/// where only root can make a name.
#[test]
fn both_units_run_as_root_from_roots_tree() {
    for unit in ["cluster-deploy-runner", "disk-floor-sweep"] {
        let text = read(&format!("infra/forge/{unit}.service"));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.contains(&"User=root"), "{unit} runs as root");
        for l in lines.iter().filter(|l| l.starts_with("Exec")) {
            let cmd = l
                .split_once('=')
                .unwrap()
                .1
                .trim_start_matches('-')
                .split_whitespace()
                .next()
                .unwrap();
            assert!(
                cmd.starts_with("/var/lib/boss/tree/current/"),
                "{unit}: {l} runs from root's tree"
            );
        }
        assert!(
            lines.contains(
                &format!("Environment=BOSS_RUN_SUMMARY_FILE=/run/{unit}.summary.json").as_str()
            ),
            "{unit}: its summary is under /run, like every root unit's"
        );
    }
    let runner = read("infra/forge/cluster-deploy-runner.service");
    assert!(runner.lines().any(|l| l == "StateDirectory=boss/converge"));
    assert!(runner.lines().any(|l| l == "StateDirectoryMode=0700"));
    let defaults = read("infra/forge/forge-defaults.sh");
    assert!(
        defaults
            .contains("CONVERGE_STATE=\"${BOSS_FORGE_CONVERGE_STATE:-/var/lib/boss/converge}\""),
        "the state directory the unit declares is the one forge-defaults.sh names"
    );
    let watchdog = read("infra/forge/cluster-watchdog.service");
    assert!(
        !watchdog
            .lines()
            .any(|l| l.starts_with("Environment=BOSS_FORGE_LAST_BUILT=")),
        "the watchdog reads forge-defaults.sh's stamp, not a path a unit hands it"
    );
}

/// THE VIEW CARRIES THE GENERATION'S HISTORY (review 1ba2873d, B1). Two
/// later stages read git in the tree they run from: the orphan check
/// walks every manifest the history deleted, and registry-drift reads
/// which side is ahead off the history and HEAD. A generation of root's
/// tree is an EXPORT — no `.git` — so in the view the first answered an
/// empty list ("orphans_check: ok", having checked nothing: a checkout
/// of the same commit finds three deleted manifests) and the second read
/// HEAD `unknown` and a shallow repository, and filed every kind it
/// could not place as "side not measured". The view now has a git
/// directory of its own whose objects are root's store (`repo.git`, a
/// full fetch) and whose HEAD is the generation's sha — written beside
/// the hard links, never into the generation or the store.
///
/// AND THE VIEW IS THIS RUN'S OWN (review 1ba2873d, N2): a hand run and a
/// timer run used to share one `run-tree` and each `rm -rf`ed the
/// other's. It is `run-tree.<pid>` now, and a run removes only the views
/// of runs no longer alive.
#[test]
fn the_view_a_run_executes_from_is_its_own_and_carries_the_generations_history() {
    let mut f = Forge::new("view-history");
    f.delete_a_manifest_on_main();
    write_file(
        &f.root.join("oldhome/.boss-last-built"),
        &format!("{}\n", f.head7()),
    );
    // A view of a run still alive (this test process) and one of a run
    // long gone: pid_max is at most 4194304, so no process has this pid.
    let alive = f.state().join(format!("run-tree.{}", std::process::id()));
    let gone = f.state().join("run-tree.4194305");
    for v in [&alive, &gone] {
        create_dir(v);
        write_file(&v.join("marker"), "a run's view\n");
    }
    let out = f.run(&[]);
    let t = text(&out);
    assert!(
        out.status.success(),
        "main unchanged is a clean exit 0: {t}"
    );

    assert!(
        alive.join("marker").is_file(),
        "a live run's view is never removed by another run"
    );
    assert!(!gone.exists(), "a dead run's view is removed");
    let ours: Vec<PathBuf> = f.views().into_iter().filter(|v| *v != alive).collect();
    assert_eq!(ours.len(), 1, "this run made one view: {ours:?}\n{t}");
    let view = &ours[0];
    let pid = view
        .file_name()
        .unwrap()
        .to_string_lossy()
        .strip_prefix("run-tree.")
        .map(str::to_string)
        .unwrap_or_default();
    assert!(
        !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()),
        "the view is named by the run's pid: {}",
        view.display()
    );

    // Git, run in the view as the two stages run it, answers for the
    // generation with the whole history behind it.
    assert_eq!(git(view, &["rev-parse", "HEAD"]), f.head);
    assert_eq!(
        git(view, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    let deleted = git(
        view,
        &[
            "log",
            "-m",
            "--first-parent",
            "--diff-filter=D",
            "--name-only",
            "--format=",
            "--",
            "infra/cluster/manifests",
        ],
    );
    assert!(
        deleted
            .lines()
            .any(|l| l == "infra/cluster/manifests/gone.yaml"),
        "the orphan check's own query sees the deletion from the view: {deleted:?}"
    );
    assert_eq!(
        git(
            view,
            &[
                "diff",
                "--diff-filter=D",
                "--name-only",
                "HEAD",
                "--",
                "infra"
            ]
        ),
        "",
        "the view is the generation, whole: nothing reads as deleted in it"
    );
    // Written beside the hard links, never into what root's tree keeps.
    let generation = f.root.join("tree/gen").join(&f.head);
    assert!(generation.is_dir());
    assert!(
        !generation.join(".git").exists(),
        "the generation stays an export"
    );
}

/// THE ALERT SPOOL IS THE CONVERGE'S OWN WHOEVER STARTS IT (review
/// 1ba2873d, B2). The converge replays every body in its alert spool to
/// the jobs API on every tick, as root, presenting the machine token. Only
/// the unit named that spool; started without it — the hand run the
/// recovery sheet used to describe — alert-lib.sh fell back to
/// /var/tmp/boss-alert-spool, sticky and world-writable, so any account
/// on the forge could plant a body root then filed as platform-admin.
/// Without ALERT_SPOOL in its environment the runner now keeps and
/// replays under its own root-only state directory.
#[test]
fn a_run_started_without_the_units_environment_replays_only_its_own_spool() {
    let f = Forge::new("spool-default");
    write_file(
        &f.root.join("oldhome/.boss-last-built"),
        &format!("{}\n", f.head7()),
    );
    let spool = f.state().join("alert-spool");
    create_dir(&spool);
    write_file(
        &spool.join("2026-10-09T00:00:00Z-1-1-1.json"),
        "{\"title\":\"kept-by-an-earlier-tick\"}",
    );
    // The alert door asks for JOBS_API by that name (alert-lib.sh); the
    // unit's environment file names both spellings.
    let out = f.run_without(&[("JOBS_API", "http://stub:7900")], &["ALERT_SPOOL"]);
    let t = text(&out);
    assert!(
        out.status.success(),
        "main unchanged is a clean exit 0: {t}"
    );
    let calls = f.calls();
    assert!(
        calls.contains("kept-by-an-earlier-tick"),
        "the run replayed the spool under its own state directory: {calls}\n{t}"
    );
}

/// The unit still NAMES the spool — infra/forge/probe-account.sh reads
/// every installed unit's `ALERT_SPOOL=` to prove the probe account
/// cannot make or write it — so the path lives twice: there, and as the
/// runner's default. A fact that lives twice gets an equality test
/// (CLAUDE.md §9a).
#[test]
fn the_units_alert_spool_is_the_runners_default() {
    let unit = read("infra/forge/cluster-deploy-runner.service");
    let named: Vec<&str> = unit
        .lines()
        .filter_map(|l| l.strip_prefix("Environment=ALERT_SPOOL="))
        .collect();
    assert_eq!(named.len(), 1, "the unit names one alert spool");
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\" && printf '%s' \"$CONVERGE_ALERT_SPOOL\"")
        .arg("bash")
        .arg(repo_root().join("infra/forge/forge-defaults.sh"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_SOR_ENV", "/nonexistent/sor.env")
        .output()
        .expect("bash runs");
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        named[0],
        "the unit's spool is forge-defaults.sh's CONVERGE_ALERT_SPOOL"
    );
    let runner = read(RUNNER);
    assert!(
        live(&runner)
            .iter()
            .any(|l| l.trim() == "ALERT_SPOOL=\"${ALERT_SPOOL:-$CONVERGE_ALERT_SPOOL}\""),
        "the runner defaults to it before anything replays"
    );
}
