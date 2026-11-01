//! `publish-tenant.sh` — a tenant publish into the live registries rides
//! a passkey-signed plan, never an operator's terminal (backlog 1610115d).
//!
//! WHY. David ran `boss tenant publish --take departments` against prod
//! on 2026-09-25 and `--take classes` on 2026-09-26, by hand, with a
//! dry-run and the operator between the file and the registry: no
//! packet, no approval, no record of what the live rows were before the
//! overwrite. The door pinned here is design 17835005's shape (David,
//! 2026-09-21: "a rendered plan hash, signed with his passkey,
//! single-use, verified before the argv is built"), the third caller
//! after commission-a-disk and merge-tenant-main: `plan-a-tenant-publish`
//! renders, `publish-tenant` publishes exactly the plan whose hash it is
//! handed, and the plan carries the LIVE registries — so an approval
//! signed over one live state cannot publish over another.
//!
//! THE FIXTURE. The tenant is a bare repo on a forge on disk, found the
//! way the converge finds it (`BOSS_FORGE_COMPOSE` → the compose file's
//! /data → `git/repositories/david/algedonic-llc.git`, the repo the real
//! `infra/cluster/instances.toml` gives prod). The CLI is a stub named by
//! `BOSS_CLI`: `tenant check` passes unless the tree says BROKEN, the
//! dry-run refuses a take named `bogus` the way the CLI's own parser
//! does and names each file's take as the real one does, `tenant export`
//! copies a directory standing in for the live registries (in the
//! export's canonical layout) into the copy it is run in and reports
//! each file it read, the publish copies each taken registry's seed file
//! into that directory, and every call logs a timestamped line to stderr
//! as the real CLI does. The real CLI's check, publish and export are
//! pinned in boss-cli; what is pinned here is that the verb RUNS them in
//! this order, on these trees, compares them row by row, and holds the
//! write to the plan's bytes. Review c757cf5d's four findings each have
//! a test below that failed before its fix.

use std::path::{Path, PathBuf};
use std::process::Command;

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};

fn script() -> PathBuf {
    repo_root().join("infra/forge/publish-tenant.sh")
}

const DOOR: &str = "http://192.0.2.7:7900";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(git_env())
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

/// Identity and isolation for every git this file runs — the gate's
/// uid has no usable HOME.
fn git_env() -> Vec<(&'static str, String)> {
    vec![
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_AUTHOR_NAME", "fixture".into()),
        ("GIT_AUTHOR_EMAIL", "fixture@example.invalid".into()),
        ("GIT_COMMITTER_NAME", "fixture".into()),
        ("GIT_COMMITTER_EMAIL", "fixture@example.invalid".into()),
    ]
}

fn put(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    write_file(p, body);
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    bare: PathBuf,
    live: PathBuf,
    compose: PathBuf,
    sor_env: PathBuf,
    cli: PathBuf,
    log: PathBuf,
}

/// The stand-in CLI. Every invocation is logged; `export` and the real
/// publish act on `$STUB_LIVE`, the instance's registries.
///
/// It logs to stderr the way the real CLI does on every call that builds
/// a machine client: a timestamped INFO line (boss-cli main.rs's tracing
/// subscriber, `machine_token.rs` "machine token appeared"), never the
/// same twice. Review c757cf5d F1: the first stub wrote nothing there,
/// so the determinism test passed on a property the real CLI does not
/// have, while two real exports of one state differed in that line.
const STUB: &str = r#"#!/usr/bin/env bash
echo "$*" >> "$STUB_LOG"
echo "$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)  INFO boss_core::machine_token: machine token appeared dir=/etc/boss-doors/machine-token" >&2
[ "$1" = tenant ] || exit 2
case "$2" in
check)
    echo "boss tenant check $3"
    if grep -rq BROKEN .; then echo '0 ok, 0 missing, 1 invalid, 0 unknown — FAIL'; exit 1; fi
    echo '2 ok, 0 missing, 0 invalid, 0 unknown — PASS' ;;
export)
    [ -d "$STUB_LIVE" ] || { echo "GET /api/classes: connection refused" >&2; exit 1; }
    echo "boss tenant export $3 (tenant_id algedonic)"
    cp -r "$STUB_LIVE"/. .
    # The real report's line per file it read (tenant_export.rs
    # render_changes); a registry it has no read of (departments) is in
    # no line, as in the real one.
    (cd "$STUB_LIVE" && find . -type f | sed 's#^\./##' | sort) | while read -r f; do
        printf '  %-9s %-24s  1 row(s)\n' changed "$f"
    done
    echo "0 added, 1 changed, 0 unchanged, 0 skipped — the instance is the truth; this directory is its bootstrap" ;;
publish)
    take=none; dry=0
    for a in "$@"; do
        case "$prev" in --take) take="$a" ;; esac
        [ "$a" = --dry-run ] && dry=1
        prev="$a"
    done
    case ",$take," in *,bogus,*) echo "REFUSED: --take bogus: no door overwrites that registry" >&2; exit 1 ;; esac
    if [ "$dry" = 1 ]; then
        # The real dry-run's line per file, which names the take that
        # overwrites it (tenant_publish.rs).
        echo "boss tenant publish $3 (tenant_id algedonic) — dry-run, no HTTP"
        echo "take: $take"
        echo "  seeds/classes.json      POST /api/classes/batch  1 classes (insert-if-absent; a held row that differs is named; --take classes edits it)"
        echo "  seeds/departments.toml  POST /api/departments/batch  1 departments (insert-if-absent by code: it; a held row that differs is named; --take departments applies the declaration, retired = true included)"
        echo "  seeds/locations.toml    POST /api/locations/batch  1 locations (insert-if-absent by id: loc-hq)"
        echo "  seeds/agents.toml       POST /api/agents/batch  1 agents (by id: agent-a; a registered row is kept and its differing declared fields are named; --take agents applies the whole declaration; an undeclared alias is kept either way)"
        exit 0
    fi
    echo "boss tenant publish $3 (tenant_id algedonic)"
    for r in ${take//,/ }; do
        [ "$r" = none ] && continue
        [ -n "${STUB_NOOP:-}" ] && continue
        for f in seeds/"$r".*; do [ -f "$f" ] && cp "$f" "$STUB_LIVE/$f" && echo "  took $f"; done
    done
    if [ -n "${STUB_SPOIL:-}" ]; then
        printf '[[location]]\nid = "loc-hq"\nname = "Moved during the publish"\ntimezone = "UTC"\n' > "$STUB_LIVE/seeds/locations.toml"
    fi ;;
*) exit 2 ;;
esac
"#;

/// Tenant main, hand-written: comments, its own key order and quoting.
/// The instance, in the export's canonical form. classes and agents each
/// hold one row the tree declares differently (the drift a take resolves
/// one registry at a time), classes one row the tree does not declare (a
/// platform row), and locations the SAME row in another layout.
/// departments is in the tree and the export has no read of it.
const TREE_CLASSES: &str = "[\n  { \"subject_kind\": \"employee\", \"code\": \"engineer\", \"display_name\": \"Engineer (tree)\" }\n]\n";
const TREE_AGENTS: &str = "# The agents the company runs.\n[[agent]]\nid = \"agent-a\"\nrole = \"builder\"\ndisplay_name = \"Agent A\"\n";
const TREE_LOCATIONS: &str = "# One office. Hand-written: this comment, the key order and the\n# quoting are not the export's.\n[[location]]\nname = 'HQ'\nid = \"loc-hq\"\n\ntimezone = \"UTC\"\n";
const LIVE_CLASSES: &str = "[\n  {\n    \"code\": \"engineer\",\n    \"display_name\": \"Engineer (live)\",\n    \"subject_kind\": \"employee\"\n  },\n  {\n    \"code\": \"operator\",\n    \"display_name\": \"Platform operator\",\n    \"subject_kind\": \"employee\"\n  }\n]\n";
const LIVE_AGENTS: &str = "# [[agent]] rows exported from the instance by `boss tenant export`.\n\n[[agent]]\ndisplay_name = \"Agent A\"\nid = \"agent-a\"\nrole = \"reviewer\"\n";
const LIVE_LOCATIONS: &str = "# [[location]] rows exported from the instance by `boss tenant export`.\n\n[[location]]\nid = \"loc-hq\"\nname = \"HQ\"\ntimezone = \"UTC\"\n";

fn fixture() -> Fixture {
    let root = scratch_dir("publish-tenant");
    let bare = root.join("forge/david/algedonic-llc.git");
    let work = root.join("work");
    let live = root.join("live");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);

    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    put(
        &work.join("tenant.toml"),
        "[meta]\ntenant_id = \"algedonic\"\n",
    );
    put(&work.join("seeds/classes.json"), TREE_CLASSES);
    put(&work.join("seeds/agents.toml"), TREE_AGENTS);
    put(&work.join("seeds/locations.toml"), TREE_LOCATIONS);
    put(
        &work.join("seeds/departments.toml"),
        "[[department]]\ncode = \"it\"\ndisplay_name = \"IT\"\n",
    );
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "tenant main"]);
    git(&work, &["remote", "add", "forge", bare.to_str().unwrap()]);
    git(&work, &["push", "-q", "forge", "main"]);

    put(
        &live.join("tenant.toml"),
        "[meta]\ntenant_id = \"algedonic\"\n",
    );
    put(&live.join("seeds/classes.json"), LIVE_CLASSES);
    put(&live.join("seeds/agents.toml"), LIVE_AGENTS);
    put(&live.join("seeds/locations.toml"), LIVE_LOCATIONS);

    // The compose file the forge's repository root is derived from.
    std::fs::create_dir_all(root.join("fdata/git")).unwrap();
    std::os::unix::fs::symlink(root.join("forge"), root.join("fdata/git/repositories")).unwrap();
    let compose = root.join("forgejo/docker-compose.yml");
    put(
        &compose,
        &format!(
            "services:\n  forgejo:\n    volumes:\n      - {}:/data\n",
            root.join("fdata").display()
        ),
    );
    let sor_env = root.join("sor.env");
    put(&sor_env, &format!("BOSS_JOBS_URL={DOOR}\n"));
    let cli = root.join("boss-stub");
    write_exec(&cli, STUB);
    let log = root.join("cli.log");
    Fixture {
        root,
        work,
        bare,
        live,
        compose,
        sor_env,
        cli,
        log,
    }
}

impl Fixture {
    fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(script())
            .args(args)
            .envs(git_env())
            .env("BOSS_FORGE_COMPOSE", &self.compose)
            .env_remove("BOSS_FORGE_REPO_PATH")
            .env("BOSS_SOR_ENV", &self.sor_env)
            .env("BOSS_CLI", &self.cli)
            .env("STUB_LIVE", &self.live)
            .env("STUB_LOG", &self.log)
            .env("TMPDIR", &self.root);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("the script runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn plan(&self, take: &str) -> (i32, String, String) {
        self.run(&["--plan", "prod", take], &[])
    }

    fn main_sha(&self) -> String {
        git(&self.bare, &["rev-parse", "refs/heads/main"])
    }

    fn live(&self, file: &str) -> String {
        std::fs::read_to_string(self.live.join(file)).unwrap()
    }

    /// The real publishes the stub was asked for (not the dry-runs).
    fn publishes(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("tenant publish") && !l.contains("--dry-run"))
            .map(str::to_string)
            .collect()
    }
}

fn file_sha256(p: &Path) -> String {
    let out = Command::new("sha256sum").arg(p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn sha256(dir: &Path, bytes: &str) -> String {
    let p = dir.join("plan-bytes");
    std::fs::write(&p, bytes).unwrap();
    let out = Command::new("sha256sum").arg(&p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn the_plan_names_the_tree_the_take_the_check_and_the_live_registries() {
    let f = fixture();
    let (code, out, err) = f.plan("classes");
    assert_eq!(code, 0, "plan refused:\n{out}\n{err}");
    let main = f.main_sha();
    let tree = git(&f.bare, &["rev-parse", "refs/heads/main^{tree}"]);
    for needle in [
        "plan: publish-tenant".to_string(),
        "instance: prod (namespace boss)".to_string(),
        "tenant repo: david/algedonic-llc".to_string(),
        format!("publishes: refs/heads/main at {main}"),
        format!("tree: {tree}"),
        format!("door: {DOOR}"),
        "take: classes — the live rows of these registries are overwritten".to_string(),
        "boss tenant check .".to_string(),
        "— PASS".to_string(),
        "dry-run, no HTTP".to_string(),
        "boss tenant export . (tenant_id algedonic)".to_string(),
        // The CLI build that rendered it (review c757cf5d F5): a converge
        // between the signature and the execute moves the bytes.
        format!("cli: boss, sha256 {}", file_sha256(&f.cli)),
        // The live state ROW by row. The taken registry: the row the take
        // overwrites, field by field — what the tree says (-) and what the
        // instance holds now (+); the platform row the tree does not
        // declare is counted and not shown.
        "seeds/classes.json (TAKEN): 1 declared row(s) — 0 the same live, 1 differ, 0 not live; 1 live row(s) the tree does not declare".to_string(),
        "OVERWRITTEN classes[subject_kind=employee,code=engineer]".to_string(),
        "- display_name = \"Engineer (tree)\"".to_string(),
        "+ display_name = \"Engineer (live)\"".to_string(),
        // Not taken: named, kept, not printed.
        "seeds/agents.toml: 1 declared row(s) — 0 the same live, 1 differ".to_string(),
        "differ, the publish keeps the live row (not taken): agent[id=agent-a]".to_string(),
        // The same row in another layout is the same row (F2).
        "seeds/locations.toml: 1 declared row(s) — 1 the same live, 0 differ, 0 not live".to_string(),
        // A registry the export has no read of says so (F3).
        "seeds/departments.toml: NOT READ".to_string(),
        format!("boss tenant publish . --door {DOOR} --take classes"),
    ] {
        assert!(out.contains(&needle), "the plan lacks `{needle}`:\n{out}");
    }
    for absent in ["Platform operator", "reviewer", "machine token appeared"] {
        assert!(!out.contains(absent), "the plan shows `{absent}`:\n{out}");
    }
    // The plan wrote nothing: no real publish, the instance untouched.
    assert!(f.publishes().is_empty(), "{:?}", f.publishes());
    assert_eq!(f.live("seeds/classes.json"), LIVE_CLASSES);
    // No scratch path in what is signed.
    assert!(!out.contains(f.root.to_str().unwrap()), "{out}");
    // The hash is of the plan's own bytes, and is not inside them.
    assert!(
        err.contains(&format!("plan-sha256: {}", sha256(&f.root, &out))),
        "{err}"
    );
}

#[test]
fn the_same_state_renders_the_same_bytes_and_a_moved_live_row_or_main_does_not() {
    let f = fixture();
    let (_, first, _) = f.plan("classes");
    let (_, second, _) = f.plan("classes");
    assert_eq!(
        first, second,
        "two plans of one state must be byte-identical"
    );

    // Someone edits a live row of a registry the take does not name: the
    // instance is still not what was planned.
    put(
        &f.live.join("seeds/agents.toml"),
        &LIVE_AGENTS.replace("reviewer", "auditor"),
    );
    let (code, third, _) = f.plan("classes");
    assert_eq!(code, 0);
    assert_ne!(first, third, "a live edit must move the plan's bytes");

    // ...and a row the tree does not declare, which the plan only counts.
    put(
        &f.live.join("seeds/classes.json"),
        &LIVE_CLASSES.replace("Platform operator", "Platform operator, renamed"),
    );
    let (code, edited, _) = f.plan("classes");
    assert_eq!(code, 0);
    assert_ne!(
        third, edited,
        "an undeclared live row is in the plan's digest"
    );

    // Tenant main moves.
    put(
        &f.work.join("seeds/agents.toml"),
        &TREE_AGENTS.replace("Agent A", "Agent A, v2"),
    );
    git(&f.work, &["commit", "-q", "-am", "main moved"]);
    git(&f.work, &["push", "-q", "forge", "main"]);
    let (code, fourth, _) = f.plan("classes");
    assert_eq!(code, 0);
    assert_ne!(edited, fourth, "a moved main must move the plan's bytes");
}

#[test]
fn the_write_publishes_only_the_plan_whose_hash_it_is_handed_and_reads_it_back() {
    let f = fixture();
    let (_, out, _) = f.plan("classes");
    let hash = sha256(&f.root, &out);

    let wrong = "0".repeat(64);
    let (code, _, err) = f.run(&["prod", "classes", &wrong], &[]);
    assert_eq!(code, 78, "{err}");
    assert!(f.publishes().is_empty(), "a refused write published");
    assert_eq!(f.live("seeds/classes.json"), LIVE_CLASSES);

    let (code, stdout, err) = f.run(&["prod", "classes", &hash], &[]);
    assert_eq!(code, 0, "{stdout}\n{err}");
    assert_eq!(
        f.publishes(),
        vec![format!("tenant publish . --door {DOOR} --take classes")],
        "exactly one publish, through the door, with the planned take"
    );
    assert_eq!(f.live("seeds/classes.json"), TREE_CLASSES);
    // The read-back, row by row: the taken class converged, the agent
    // (not taken) still differs, and the take left nothing unconverged.
    let effect = stdout
        .lines()
        .find(|l| l.contains("read back through the door:"))
        .unwrap_or_else(|| panic!("no read-back line:\n{stdout}"));
    assert!(
        effect.starts_with("publish-tenant: david/algedonic-llc@")
            && effect.contains("published into prod (take classes)")
            && effect.contains(
                "1 declared row(s) converged on the tree (seeds/classes.json classes[subject_kind=employee,code=engineer])"
            )
            && effect.contains("1 still differ (seeds/agents.toml agent[id=agent-a])")
            && effect.contains("0 of them in a taken registry"),
        "{effect}"
    );
    // ...and it is the line the verb file declares as its effect.
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/publish-tenant.json")).unwrap(),
    )
    .unwrap();
    let re = verb["effect"].as_str().unwrap();
    let judged = Command::new("jq")
        .args([
            "-n",
            "--arg",
            "re",
            re,
            "--arg",
            "l",
            effect,
            "$l | test($re)",
        ])
        .output();
    if let Ok(o) = judged {
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            "true",
            "{re} vs {effect}"
        );
    }
}

#[test]
fn a_live_row_edited_after_the_approval_publishes_nothing() {
    let f = fixture();
    let (_, out, _) = f.plan("classes");
    let hash = sha256(&f.root, &out);
    put(
        &f.live.join("seeds/classes.json"),
        &LIVE_CLASSES.replace("Engineer (live)", "Engineer (edited after approval)"),
    );
    let (code, stdout, err) = f.run(&["prod", "classes", &hash], &[]);
    assert_eq!(code, 78, "{stdout}\n{err}");
    assert!(err.contains("Nothing was published"), "{err}");
    assert!(f.publishes().is_empty(), "a stale approval published");
}

#[test]
fn a_row_that_differs_only_after_the_publish_is_failed() {
    let f = fixture();
    let (_, out, _) = f.plan("classes");
    let hash = sha256(&f.root, &out);
    let (code, stdout, err) = f.run(&["prod", "classes", &hash], &[("STUB_SPOIL", "1")]);
    assert_eq!(code, 1, "{stdout}\n{err}");
    assert!(
        err.contains("FAILED") && err.contains("seeds/locations.toml location[id=loc-hq]"),
        "{err}"
    );
    assert!(
        !stdout.contains("read back through the door:"),
        "no effect line on a failed read-back:\n{stdout}"
    );
}

/// Review c757cf5d F4: a taken row that is still not the tree's after
/// the publish is named apart from the rows no take touched, so the
/// effect line can tell a take that landed from one that did not.
#[test]
fn a_taken_row_the_publish_did_not_converge_is_named() {
    let f = fixture();
    let (_, out, _) = f.plan("classes");
    let hash = sha256(&f.root, &out);
    let (code, stdout, err) = f.run(&["prod", "classes", &hash], &[("STUB_NOOP", "1")]);
    assert_eq!(code, 0, "{stdout}\n{err}");
    let effect = stdout
        .lines()
        .find(|l| l.contains("read back through the door:"))
        .unwrap_or_else(|| panic!("no read-back line:\n{stdout}"));
    assert!(
        effect.contains("0 declared row(s) converged on the tree")
            && effect.contains(
                "1 of them in a taken registry, which the take did not converge (seeds/classes.json classes[subject_kind=employee,code=engineer])"
            ),
        "{effect}"
    );
}

/// Review c757cf5d F2. The live side is the export's canonical rendering
/// and the tree is hand-written, so a line diff of the two differed in
/// every file of the Algedonic tenant on comments and ordering alone —
/// 210,105 bytes, over the runner's plan cap, which then refuses to
/// render the plan at all. Here 2,000 rows are the same rows in the two
/// layouts; the plan must say so, and fit under the cap.
#[test]
fn rows_that_differ_only_in_layout_are_the_same_and_the_plan_fits_the_runners_cap() {
    let f = fixture();
    let (mut tree, mut live) = (String::new(), String::from("# exported\n\n"));
    for i in 0..2000 {
        tree.push_str(&format!(
            "# Office {i}, as the founder wrote it down on the first day.\n[[location]]\nname = 'Office {i}'\nid = \"loc-{i:04}\"\ntimezone = \"UTC\"\n\n"
        ));
    }
    for i in 0..2000 {
        live.push_str(&format!(
            "[[location]]\nid = \"loc-{i:04}\"\nname = \"Office {i}\"\ntimezone = \"UTC\"\n\n"
        ));
    }
    put(&f.work.join("seeds/locations.toml"), &tree);
    git(&f.work, &["commit", "-q", "-am", "two thousand offices"]);
    git(&f.work, &["push", "-q", "forge", "main"]);
    put(&f.live.join("seeds/locations.toml"), &live);

    let (code, out, err) = f.plan("classes");
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("seeds/locations.toml: 2000 declared row(s) — 2000 the same live, 0 differ"),
        "{out}"
    );
    let ops_runner = std::fs::read_to_string(repo_root().join("infra/ops/ops-runner.sh")).unwrap();
    let cap: usize = ops_runner
        .lines()
        .find_map(|l| l.strip_prefix("OPS_OUTPUT_CAP=\"${OPS_OUTPUT_CAP:-"))
        .and_then(|l| l.strip_suffix("}\""))
        .expect("ops-runner.sh no longer spells OPS_OUTPUT_CAP's default — this pin reads the wrong line")
        .parse()
        .unwrap();
    assert!(
        out.len() < cap / 10,
        "a plan of 2000 unchanged rows is {} bytes (the runner refuses over {cap})",
        out.len()
    );
}

/// Review c757cf5d F3. `boss tenant export` has no read of departments,
/// so a take of it would overwrite live rows the signed plan neither
/// showed nor guarded — and the plan said "no difference". Departments
/// was the 2026-09-25 hand publish this verb replaces; it is refused
/// until the export reads it, rather than signed blind.
#[test]
fn a_take_of_a_registry_the_export_does_not_read_is_refused() {
    let f = fixture();
    for take in ["departments", "classes,departments"] {
        let (code, out, err) = f.plan(take);
        assert_eq!(code, 78, "{take}: {out}\n{err}");
        assert!(out.is_empty(), "{take}: a plan was rendered:\n{out}");
        assert!(
            err.contains("--take departments")
                && err.contains("seeds/departments.toml")
                && err.contains("does not read"),
            "{take}: {err}"
        );
    }
    // Not taken, the same file is named as unread and the plan renders.
    let (code, out, err) = f.plan("none");
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("seeds/departments.toml: NOT READ"), "{out}");
    assert!(f.publishes().is_empty());
}

#[test]
fn a_refused_take_a_failing_tree_and_an_unreadable_instance_render_no_plan() {
    let f = fixture();
    // A take the CLI's own parser refuses.
    let (code, out, err) = f.plan("classes,bogus");
    assert_eq!(code, 78, "{out}\n{err}");
    assert!(out.is_empty() && err.contains("bogus"), "{out}\n{err}");

    // An instance whose registries cannot be read: no plan, exit 1.
    let (code, out, err) = f.run(
        &["--plan", "prod", "classes"],
        &[("STUB_LIVE", f.root.join("nowhere").to_str().unwrap())],
    );
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(out.is_empty(), "a plan without the live state:\n{out}");
    assert!(err.contains("NOT read"), "{err}");

    // A tree that does not pass its check.
    put(&f.work.join("seeds/classes.json"), "BROKEN\n");
    git(&f.work, &["commit", "-q", "-am", "break it"]);
    git(&f.work, &["push", "-q", "forge", "main"]);
    let (code, out, err) = f.plan("classes");
    assert_eq!(code, 78, "{out}\n{err}");
    assert!(out.is_empty() && err.contains("FAIL"), "{out}\n{err}");
}

#[test]
fn an_instance_the_door_does_not_reach_is_refused() {
    let f = fixture();
    // The playground is image-sourced; an unknown name is no instance;
    // a malformed take is refused before anything is read.
    for args in [
        ["--plan", "playground", "none"],
        ["--plan", "nowhere", "none"],
        ["--plan", "prod", "Classes"],
    ] {
        let (code, out, err) = f.run(&args, &[]);
        assert_eq!(code, 78, "{args:?}: {out}\n{err}");
        assert!(out.is_empty(), "{args:?}: {out}");
    }
    assert!(f.publishes().is_empty());
}

/// The structural property: `--plan` exits before the only line that
/// publishes. Code lines only — the header describes the publish.
#[test]
fn the_plan_branch_exits_before_the_publish() {
    let body = std::fs::read_to_string(script()).unwrap();
    let code: Vec<(usize, &str)> = body
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with('#')
        })
        .collect();
    let exit = code
        .iter()
        .find(|(_, l)| l.contains("PLAN") && l.contains("exit 0"))
        .map(|(i, _)| *i)
        .expect("the --plan branch no longer exits");
    let publish = code
        .iter()
        .find(|(_, l)| {
            l.contains("tenant publish . --door")
                && !l.contains("--dry-run")
                && !l.trim_start().starts_with("echo ")
        })
        .map(|(i, _)| *i)
        .expect("the script no longer publishes — this pin reads the wrong file");
    assert!(
        exit < publish,
        "--plan exits at line {} after the publish at line {}",
        exit + 1,
        publish + 1
    );
}

#[test]
fn the_plan_verb_needs_no_approval_and_the_write_verb_does() {
    let read = |n: &str| -> serde_json::Value {
        let p = repo_root().join(format!("infra/ops/verbs/{n}.json"));
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    };
    let plan = read("plan-a-tenant-publish");
    let write = read("publish-tenant");
    assert_ne!(
        plan["requires_approval"], true,
        "the read-only half runs unsigned"
    );
    assert!(!plan["about"].as_str().unwrap().contains("MUTATING"));
    assert_eq!(
        write["requires_approval"], true,
        "the write waits for a passkey"
    );
    assert_eq!(write["plan_verb"], "plan-a-tenant-publish");
    assert_eq!(write["approvers"], serde_json::json!(["emp-david"]));
    assert_eq!(plan["argv"][0], "infra/forge/publish-tenant.sh");
    assert_eq!(plan["argv"][1], "--plan");
    assert_eq!(write["argv"][0], "infra/forge/publish-tenant.sh");
    let names = |v: &serde_json::Value| -> Vec<String> {
        v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect()
    };
    let (pn, wn) = (names(&plan), names(&write));
    assert_eq!(wn.last().map(String::as_str), Some("plan_sha256"));
    assert_eq!(
        pn,
        wn[..wn.len() - 1],
        "the plan is rendered from exactly the write's args"
    );
    // The repo and the door are never parameters.
    assert!(
        !wn.iter()
            .any(|n| n.contains("repo") || n.contains("url") || n.contains("door")),
        "{wn:?}"
    );
}
