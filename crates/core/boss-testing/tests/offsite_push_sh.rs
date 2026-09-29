//! `infra/forge/offsite-push.sh` — the forge converge pushes forge `main`
//! to the PRIVATE disaster-recovery copy (algedonic-dev/boss-dr) with its
//! OWN credential slot, with a PLAIN push — no force, no `--mirror`, no
//! prune — reads the push back, and only then removes Forgejo's own push
//! mirror (backlog 21d54f4a, decided 2026-09-26 under David's rule that
//! no mirror can wipe what it mirrors). The public fork (dauld/boss-mirror)
//! is declared with NO branch.
//!
//! Forge `main` left the fork on 2026-09-27 (backlog 67931115, design
//! 1f35a3e8): dauld/boss-mirror is a fork of the public algedonic-dev/boss,
//! a fork of a public repository is public and cannot be made private,
//! and so every train reached public GitHub within one converge tick —
//! unsigned, and outside the publish flow's secrets scan and passkey.
//! `publish/*` left it next (backlog a2b58aab): a forge branch name
//! vouches for nothing, and the publish verb pushes the fork itself. Main's
//! off-site copy moved to a PRIVATE repository outside the fork network,
//! algedonic-dev/boss-dr, read with a token of its own (backlog 761bc8a9).
//! Which remote may carry which branch is pinned by
//! `the_public_fork_receives_nothing_from_the_converge` and
//! `the_dr_copy_is_declared_main_only`. The push machinery is exercised
//! with a FIXTURE declaration that gives the fork target `publish/*`, so
//! two targets, two slots and two helpers are still driven end to end;
//! `the_real_declaration_pushes_main_to_the_dr_copy_and_nothing_to_the_fork`
//! runs the real one.
//!
//! Why the Forgejo push mirror has to go rather than be filtered: v16.0.2
//! ALWAYS syncs with `git push -f --mirror` (modules/git/repo.go Push),
//! so a filtered mirror still prunes the target, and one added without a
//! filter (`git remote add --mirror`, fetch `+refs/*:refs/*`) wrote a
//! stale main back onto the forge's own refs on 2026-09-25, un-merging
//! train #687 (triage a53e92a1 reproduced both).
//!
//! Pinned here, with real git repositories standing in for the forge and
//! for GitHub, and a stub curl keeping Forgejo's push-mirror list:
//!   * main reaches the DR copy and ONLY it — the fork's main is never
//!     moved — and a fixture publish/* reaches the fork and only it; a branch
//!     outside the declaration reaches neither, and a branch only a
//!     TARGET holds survives (no prune), whether or not it matches
//!   * the declaration refuses main on two targets, main riding with
//!     anything else, no main at all, and two targets sharing a remote
//!     or a credential slot — so main cannot come back to the fork by a
//!     one-word edit; a refused declaration fetches and pushes nothing,
//!     still removes the Forgejo mirror, and exits 2
//!   * an EMPTY or absent DR credential slot is a refusal (exit 4, named,
//!     nothing pushed to either target), never a skip
//!   * a private clone still holding a branch an old declaration fetched
//!     neither pushes it nor reports it: the verdict names what it pushed
//!   * the forge's repository is only READ: its refs and config are the
//!     same after a run — this is the writer that rewound main, gone
//!   * a rewound forge main, or a non-fast-forward publish/<date>, is
//!     REFUSED, named, exit 1, and the target keeps what it had — never
//!     overwritten
//!   * the Forgejo mirror is deleted on every run that can reach the
//!     forge API — refusals, an empty or loose slot, a failed push or
//!     read-back included (review of fcf6042f, F1); only a missing forge
//!     header or an unreadable mirror list keeps it; a refused or ignored
//!     delete is exit 1
//!   * no GitHub token, a world-readable one, or one root does not own:
//!     exit 4, nothing pushed, the mirror still removed; no forge
//!     credential: exit 4 and nothing touched
//!   * a declaration value with a trailing newline is refused (jq's `$`
//!     matched before it), and a run that stops without a verdict still
//!     writes one, beginning REFUSED:
//!   * a system gitconfig cannot redirect the push or run a hook; the
//!     push and read-back carry a low-speed bound; each credential helper
//!     answers only https://github.com AND only its own target's path, so
//!     neither token can reach the other repository; a nameless mirror
//!     row is an error
//!   * the declaration names algedonic-dev/boss-dr for main alone, and the
//!     canonical dauld/boss-mirror (the fork the publish verb opens its
//!     PRs from, with the publish verb's token file) for NO branch; run
//!     for real, it pushes main to the DR copy, nothing to the fork, and
//!     needs no fork token
//!   * forge-converge.sh runs it after protect-main.sh, and its verdict
//!     reaches the packet

use boss_testing::repo_root;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const GH_TOKEN: &str = "ghp-offsite-token-must-never-print-0123456789";
const DR_TOKEN: &str = "github_pat-dr-token-must-never-print-5555555555";
const FJ_TOKEN: &str = "fj-token-must-never-print-9876543210";
/// The two credential slots, by the registry id each target names.
const DR_CREDENTIAL: &str = "github-dr-push-token";
/// The two remotes as declared. The fixture keeps them verbatim and maps
/// https://github.com/ to a local directory through the script's
/// BOSS_OFFSITE_GITHUB_BASE seam, so every verdict names these.
const PUBLIC_FORK: &str = "https://github.com/dauld/boss-mirror.git";
const PRIVATE_DR: &str = "https://github.com/algedonic-dev/boss-dr.git";
const FORK_CREDENTIAL: &str = "dauld-github-token";
const MIRROR_NAME: &str = "remote_mirror_PDxRD-8iuiw";
const LIST_URL: &str = "http://forge.test:3000/api/v1/repos/david/boss/push_mirrors";

fn script() -> PathBuf {
    repo_root().join("infra/forge/offsite-push.sh")
}

fn declaration() -> serde_json::Value {
    let text = std::fs::read_to_string(repo_root().join("infra/forge/offsite-push.json"))
        .expect("infra/forge/offsite-push.json is the declaration");
    serde_json::from_str(&text).expect("the declaration is JSON")
}

/// One git command in `dir`, with an identity so commits work; trimmed
/// stdout, or a panic carrying git's own words.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
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

fn init_bare(path: &Path) {
    let st = Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("git runs");
    assert!(st.success(), "git init --bare {}", path.display());
}

/// A commit on `parent` (or a root commit), made in `repo` without a
/// working tree.
fn commit(repo: &Path, parent: Option<&str>, msg: &str) -> String {
    let tree = git_in(repo, &["hash-object", "-t", "tree", "-w", "--stdin"]);
    match parent {
        Some(p) => git_in(repo, &["commit-tree", &tree, "-p", p, "-m", msg]),
        None => git_in(repo, &["commit-tree", &tree, "-m", msg]),
    }
}

/// `refname sha` lines for every ref under refs/heads, sorted.
fn heads(repo: &Path) -> Vec<String> {
    let out = git_in(
        repo,
        &["for-each-ref", "--format=%(refname) %(objectname)", "refs/"],
    );
    let mut v: Vec<String> = out.lines().map(str::to_string).collect();
    v.sort();
    v
}

fn head_of(repo: &Path, name: &str) -> Option<String> {
    heads(repo).iter().find_map(|l| {
        l.strip_prefix(&format!("refs/heads/{name} "))
            .map(str::to_string)
    })
}

/// The stub curl. The script calls it in ONE shape:
///   curl -sS -m <s> -o <out> -w %{http_code} -H @<auth> -X <M> <url>
/// It records `<M> <url>` to `$STUB_LOG` and keeps Forgejo's push-mirror
/// list in `$STUB_DIR/mirrors.json`:
///   GET    <list>          -> mirrors.json, 200
///   DELETE <list>/<name>   -> drops that entry, 204
/// Faults: `$STUB_DIR/fail` (curl exits 7), `$STUB_DIR/status_<M>`
/// (answer that status and change nothing), `$STUB_DIR/ignore_writes`
/// (answer 204 to a DELETE and change nothing).
fn write_curl_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("curl");
    boss_testing::write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
out=""; method=GET; url=""; auth=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -X) method="$2"; shift 2 ;;
        -H) case "$2" in @*) auth="${2#@}" ;; esac; shift 2 ;;
        -m|-w) shift 2 ;;
        -*) shift ;;
        *) url="$1"; shift ;;
    esac
done
{ echo "$method $url"; echo '=== call ==='; } >> "$STUB_LOG"
[ -e "$STUB_DIR/fail" ] && { echo "curl: (7) Failed to connect to forge port 3000" >&2; exit 7; }
grep -q '^Authorization: token ' "$auth" || { echo '{"message":"no token"}' > "$out"; printf 401; exit 0; }
if [ -e "$STUB_DIR/status_$method" ]; then
    echo '{"message":"stub refusal"}' > "$out"; cat "$STUB_DIR/status_$method"; exit 0
fi
[ -e "$STUB_DIR/empty_get" ] && [ "$method" = GET ] && { : > "$out"; printf 200; exit 0; }
list="$STUB_DIR/mirrors.json"
[ -e "$list" ] || echo '[]' > "$list"
case "$method" in
    GET) cp "$list" "$out"; printf 200 ;;
    DELETE)
        name="${url##*/}"
        [ -e "$STUB_DIR/ignore_writes" ] || { jq --arg n "$name" 'map(select(.remote_name != $n))' "$list" > "$list.new" && mv "$list.new" "$list"; }
        : > "$out"; printf 204 ;;
esac
"#,
    );
    stub
}

/// The forge's push mirror as Forgejo 16.0.2 lists it (the live one,
/// measured on packet 21d54f4a: sync_on_commit, 8h, no filter, the old
/// boss-fork URL GitHub redirects to boss-mirror).
fn live_mirror() -> serde_json::Value {
    serde_json::json!([{
        "repo_name": "boss",
        "remote_name": MIRROR_NAME,
        "remote_address": "https://github.com/dauld/boss-fork.git",
        "branch_filter": "",
        "sync_on_commit": true,
        "interval": "8h0m0s"
    }])
}

struct World {
    dir: PathBuf,
    forge: PathBuf,
    /// The public fork (dauld/boss-mirror): publish/* only.
    target: PathBuf,
    /// The private DR copy (algedonic-dev/boss-dr): main only. New and empty.
    dr: PathBuf,
    state: PathBuf,
    /// forge main's two commits: `a` then `b` (b's parent is a).
    a: String,
    b: String,
    publish: String,
}

/// The forge holds main at `b`, publish/2026-09-25, and a feature branch.
/// The fork (GitHub) holds main at `a` — where the old push left it — a
/// branch of its own, and a publish branch the forge no longer has. The
/// DR copy is the new private repository: empty.
fn world(case: &str) -> World {
    let dir = boss_testing::scratch_dir(&format!("offsite-push-{case}"));
    let forge = dir.join("forge/david/boss.git");
    let target = dir.join("github/dauld/boss-mirror.git");
    let dr = dir.join("github/algedonic-dev/boss-dr.git");
    std::fs::create_dir_all(forge.parent().unwrap()).unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    init_bare(&forge);
    init_bare(&target);
    init_bare(&dr);
    let a = commit(&forge, None, "a");
    let b = commit(&forge, Some(&a), "b");
    let publish = commit(&forge, Some(&a), "publish snapshot");
    let feat = commit(&forge, Some(&b), "feature");
    git_in(&forge, &["update-ref", "refs/heads/main", &b]);
    git_in(
        &forge,
        &["update-ref", "refs/heads/publish/2026-09-25", &publish],
    );
    git_in(&forge, &["update-ref", "refs/heads/feat/forge-only", &feat]);
    // The target starts where the old mirror left it, one merge behind.
    git_in(
        &forge,
        &[
            "push",
            "-q",
            target.to_str().unwrap(),
            &format!("{a}:refs/heads/main"),
            &format!("{a}:refs/heads/gh-only"),
            &format!("{a}:refs/heads/publish/2026-09-01"),
        ],
    );
    let state = dir.join("stub");
    boss_testing::create_dir(&state);
    World {
        dir,
        forge,
        target,
        dr,
        state,
        a,
        b,
        publish,
    }
}

/// The REAL declaration's targets, remotes and branches verbatim, with
/// only each token file aimed at a fixture file. The remotes reach the
/// fixture repositories through BOSS_OFFSITE_GITHUB_BASE (`github/` in
/// the world's dir, laid out owner/name.git as GitHub is). Keyed by the
/// credential id each target names, so a target the tests do not know is
/// a panic, not a silent pass.
fn real_targets(w: &World) -> serde_json::Value {
    let mut d = declaration();
    let targets = d["targets"]
        .as_array_mut()
        .expect("the declaration carries a targets list");
    for t in targets.iter_mut() {
        let token = match t["credential"].as_str() {
            Some(DR_CREDENTIAL) => w.dir.join("github-dr.token"),
            Some(FORK_CREDENTIAL) => w.dir.join("github.token"),
            other => panic!("a target these tests have no fixture for: {other:?}"),
        };
        t["token_file"] = serde_json::json!(token.to_str().unwrap());
    }
    serde_json::Value::Array(targets.clone())
}

/// The fixture declaration the push machinery runs under: the real
/// targets, with the public fork given `publish/*`. The REAL declaration
/// carries nothing to the fork (backlog a2b58aab, pinned by
/// `the_public_fork_receives_nothing_from_the_converge`); the fixture
/// keeps a second target pushing, so two slots, two helpers and a refusal
/// on one target beside a landing on the other are still exercised.
fn fixture_targets(w: &World) -> serde_json::Value {
    let mut t = real_targets(w);
    let fork = target_index(&t, FORK_CREDENTIAL);
    t[fork]["branches"] = serde_json::json!(["publish/*"]);
    t
}

/// The index, in `targets`, of the target naming `credential`.
fn target_index(targets: &serde_json::Value, credential: &str) -> usize {
    targets
        .as_array()
        .unwrap()
        .iter()
        .position(|t| t["credential"] == credential)
        .unwrap_or_else(|| panic!("no target names {credential}"))
}

#[derive(Default)]
struct Opts {
    mirrors: Option<serde_json::Value>,
    fail: bool,
    status: Option<(&'static str, &'static str)>,
    ignore_writes: bool,
    /// The credential slot (by registry id) whose token file is absent.
    no_token: Option<&'static str>,
    /// The credential slot whose token file exists and is EMPTY.
    empty_token: Option<&'static str>,
    token_mode: Option<u32>,
    no_forge_auth: bool,
    /// Replaces the declaration's `targets` (after the fixture has aimed
    /// them): the declaration tests edit what fixture_targets returns.
    targets: Option<serde_json::Value>,
    /// Run the REAL declaration's branches (the fork declaring none)
    /// rather than the fixture's publish/* for the fork.
    real_declaration: bool,
    /// Removes the DR copy's stand-in repository, so main's push fails.
    dead_target: bool,
    /// Leaves BOSS_FORGE_URL unset, so sor.sh refuses before any verdict.
    no_forge_url: bool,
    /// The forge answers the mirror list 200 with an empty body.
    empty_get: bool,
    /// A system gitconfig (its body) the run can see: GIT_CONFIG_SYSTEM
    /// names it and GIT_CONFIG_NOSYSTEM is NOT set, the way a host's
    /// /etc/gitconfig reaches a root unit.
    system_gitconfig: Option<String>,
    /// The token file is owned by an account other than the one the
    /// script is told must own it (root on the forge host).
    foreign_token_owner: bool,
    /// Replaces the stand-in the run is handed for https://github.com/
    /// (by default the world's `github/` directory).
    github_base: Option<String>,
    /// More environment for the script, as a unit's Environment= would
    /// set it.
    env: Vec<(String, String)>,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    log: String,
    /// Every git invocation the script made, argv one per line, each
    /// followed by `=== git ===`.
    git_log: String,
    mirrors: serde_json::Value,
    summary: Option<serde_json::Value>,
}

/// The real git, found on PATH before the recording wrapper goes in
/// front of it.
fn real_git() -> PathBuf {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Path::new(d).join("git"))
        .find(|p| p.is_file())
        .expect("git is on PATH")
}

/// A `git` first on the script's PATH that records its argv to
/// `$OFFSITE_GIT_LOG` and hands on to the real one — so a test can read what the
/// push and the read-back were CALLED with, not what the script's text
/// happens to say.
fn write_git_wrapper(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    boss_testing::write_exec(
        &bin.join("git"),
        &format!(
            "#!/usr/bin/env bash\n{{ printf '%s\\n' \"$@\"; echo '=== git ==='; }} >> \"$OFFSITE_GIT_LOG\"\nexec '{}' \"$@\"\n",
            real_git().display()
        ),
    );
    bin
}

/// The argv of each recorded git invocation that carries `verb`.
fn git_calls<'a>(r: &'a Run, verb: &str) -> Vec<Vec<&'a str>> {
    r.git_log
        .split("=== git ===\n")
        .map(|c| c.lines().collect::<Vec<_>>())
        .filter(|argv| argv.contains(&verb))
        .collect()
}

fn run(w: &World, o: Opts) -> Run {
    let curl = write_curl_stub(&w.dir);
    let bin = write_git_wrapper(&w.dir);
    let mirrors = o.mirrors.clone().unwrap_or_else(|| serde_json::json!([]));
    boss_testing::write_file(&w.state.join("mirrors.json"), &mirrors.to_string());
    if o.empty_get {
        boss_testing::write_file(&w.state.join("empty_get"), "");
    }
    if o.fail {
        boss_testing::write_file(&w.state.join("fail"), "");
    }
    if let Some((method, status)) = o.status {
        boss_testing::write_file(&w.state.join(format!("status_{method}")), status);
    }
    if o.ignore_writes {
        boss_testing::write_file(&w.state.join("ignore_writes"), "");
    }
    // The declaration under test: the real one's targets, aimed at the
    // fixture repositories and token files.
    let targets = o.targets.clone().unwrap_or_else(|| {
        if o.real_declaration {
            real_targets(w)
        } else {
            fixture_targets(w)
        }
    });
    if o.dead_target {
        let _ = std::fs::remove_dir_all(&w.dr);
    }
    let decl = serde_json::json!({ "targets": targets });
    let decl_path = w.dir.join("offsite-push.json");
    boss_testing::write_file(&decl_path, &decl.to_string());

    for (credential, file, value) in [
        (DR_CREDENTIAL, "github-dr.token", DR_TOKEN),
        (FORK_CREDENTIAL, "github.token", GH_TOKEN),
    ] {
        let path = w.dir.join(file);
        let _ = std::fs::remove_file(&path);
        if o.no_token == Some(credential) {
            continue;
        }
        let body = if o.empty_token == Some(credential) {
            String::new()
        } else {
            format!("{value}\n")
        };
        boss_testing::write_file(&path, &body);
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(o.token_mode.unwrap_or(0o600)),
        )
        .unwrap();
    }
    let auth = w.dir.join("forge-auth-header");
    if !o.no_forge_auth {
        boss_testing::write_file(&auth, &format!("Authorization: token {FJ_TOKEN}\n"));
    }
    let log = w.dir.join("calls.log");
    let _ = std::fs::remove_file(&log);
    let git_log = w.dir.join("git.log");
    let _ = std::fs::remove_file(&git_log);
    let summary = w.dir.join("summary.json");
    let _ = std::fs::remove_file(&summary);
    // Who must own the token file: root on the forge host. The fixture's
    // file is owned by whoever runs the test, so that account is named —
    // or, for the foreign-owner case, one that is not it.
    let me = std::fs::metadata(&w.dir).unwrap().uid();
    let owner = if o.foreign_token_owner { me + 1 } else { me };
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", &w.dir)
        .env("OFFSITE_GIT_LOG", &git_log)
        .env("BOSS_OFFSITE_TOKEN_OWNER_UID", owner.to_string());
    match &o.system_gitconfig {
        Some(body) => {
            let sys = w.dir.join("system.gitconfig");
            boss_testing::write_file(&sys, body);
            cmd.env("GIT_CONFIG_SYSTEM", &sys);
        }
        None => {
            cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        }
    }
    for (k, v) in &o.env {
        cmd.env(k, v);
    }
    let github_base = o
        .github_base
        .clone()
        .unwrap_or_else(|| w.dir.join("github").to_str().unwrap().to_string());
    if !o.no_forge_url {
        cmd.env("BOSS_FORGE_URL", "http://forge.test:3000");
    }
    let out = cmd
        .env("BOSS_OFFSITE_PUSH_DECL", &decl_path)
        .env("BOSS_OFFSITE_STATE_DIR", w.dir.join("offsite-state"))
        .env("BOSS_OFFSITE_GITHUB_BASE", &github_base)
        .env("BOSS_OFFSITE_CURL", &curl)
        .env("BOSS_FORGE_REPO_PATH", &w.forge)
        .env("BOSS_SOR_ENV", w.dir.join("no-sor.env"))
        .env("BOSS_FORGE_AUTH_HEADER_FILE", &auth)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("STUB_LOG", &log)
        .env("STUB_DIR", &w.state)
        .output()
        .expect("bash runs");
    let read_json = |p: &Path| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    };
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        log: std::fs::read_to_string(&log).unwrap_or_default(),
        git_log: std::fs::read_to_string(&git_log).unwrap_or_default(),
        mirrors: read_json(&w.state.join("mirrors.json")).unwrap_or(serde_json::Value::Null),
        summary: read_json(&summary),
    }
}

fn calls(log: &str) -> Vec<String> {
    log.split("=== call ===\n")
        .filter_map(|c| c.lines().next().map(str::to_string))
        .filter(|c| !c.is_empty())
        .collect()
}

fn verdict(r: &Run) -> String {
    r.summary
        .as_ref()
        .and_then(|s| s["offsite_push"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Whether `text` names the branch main as a word — `main` or
/// `refs/heads/main` between separators — rather than as part of a
/// longer word or path.
fn names_main(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || ",;:()|".contains(c))
        .any(|t| t == "main" || t == "refs/heads/main")
}

/// The verdict's part about one target: targets are joined by " | ", and
/// each part names its remote.
fn segment_for<'a>(verdict: &'a str, remote: &str) -> &'a str {
    verdict
        .split(" | ")
        .find(|s| s.contains(remote))
        .unwrap_or_else(|| panic!("the verdict has no part about {remote}: {verdict}"))
}

fn no_token_anywhere(r: &Run) {
    for (what, text) in [
        ("stdout", &r.stdout),
        ("stderr", &r.stderr),
        ("curl argv", &r.log),
    ] {
        assert!(
            !text.contains(GH_TOKEN),
            "the GitHub token reached {what}:\n{text}"
        );
        assert!(
            !text.contains(DR_TOKEN),
            "the DR token reached {what}:\n{text}"
        );
        assert!(
            !text.contains(FJ_TOKEN),
            "the forge token reached {what}:\n{text}"
        );
    }
}

#[test]
fn main_reaches_only_the_dr_copy_and_publish_only_the_fork_and_nothing_is_pruned() {
    let w = world("first");
    let forge_before = heads(&w.forge);
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        head_of(&w.dr, "main"),
        Some(w.b.clone()),
        "main arrives on the private DR copy"
    );
    assert_eq!(
        head_of(&w.target, "main"),
        Some(w.a.clone()),
        "the fork's main is never moved: main does not go to the fork"
    );
    assert_eq!(
        head_of(&w.target, "publish/2026-09-25"),
        Some(w.publish.clone()),
        "publish/* arrives on the fork"
    );
    assert_eq!(
        heads(&w.dr),
        vec![format!("refs/heads/main {}", w.b)],
        "the DR copy holds main and nothing else"
    );
    for repo in [&w.target, &w.dr] {
        assert_eq!(
            head_of(repo, "feat/forge-only"),
            None,
            "a branch outside the declaration is not pushed"
        );
    }
    let v = verdict(&r);
    assert!(
        segment_for(&v, PRIVATE_DR).contains(&format!(
            "main to {PRIVATE_DR}: pushed main; read back at the forge's value: main"
        )),
        "the verdict names main and where it went: {v}"
    );
    let fork = segment_for(&v, PUBLIC_FORK);
    assert!(
        fork.contains("publish/*") && fork.contains("publish/2026-09-25"),
        "the fork's part names its declaration and the ref it pushed: {v}"
    );
    assert!(!names_main(fork), "the fork's part names no main: {v}");
    let fork_path = w.target.to_str().unwrap();
    assert!(
        git_calls(&r, "push")
            .iter()
            .filter(|argv| argv.contains(&fork_path))
            .flatten()
            .all(|a| !a.contains("refs/heads/main")),
        "no push to the fork names main: {}",
        r.git_log
    );
    assert_eq!(
        head_of(&w.target, "gh-only"),
        Some(w.a.clone()),
        "a branch only the target holds survives: no prune"
    );
    assert_eq!(
        head_of(&w.target, "publish/2026-09-01"),
        Some(w.a.clone()),
        "a publish branch the forge lacks survives: no prune inside the pattern either"
    );
    // The writer that rewound main on 2026-09-25 was the forge's OWN
    // repository running a push. This one only reads it.
    assert_eq!(
        heads(&w.forge),
        forge_before,
        "the forge's refs are untouched"
    );
    let cfg = std::fs::read_to_string(w.forge.join("config")).unwrap();
    assert!(
        !cfg.contains("[remote"),
        "no remote added to the forge: {cfg}"
    );
    no_token_anywhere(&r);
}

#[test]
fn the_forgejo_mirror_is_removed_only_after_the_push_reads_back() {
    let w = world("remove");
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        calls(&r.log),
        vec![
            format!("GET {LIST_URL}"),
            format!("DELETE {LIST_URL}/{MIRROR_NAME}"),
            format!("GET {LIST_URL}"),
        ],
        "list, delete, read back — after the push"
    );
    assert_eq!(
        r.mirrors,
        serde_json::json!([]),
        "no Forgejo push mirror left"
    );
    assert!(
        r.stdout.contains(MIRROR_NAME),
        "the removal is named: {}",
        r.stdout
    );
    assert!(verdict(&r).contains("removed"), "{:?}", r.summary);
    no_token_anywhere(&r);
}

#[test]
fn a_steady_tick_writes_nothing_to_the_forge() {
    let w = world("steady");
    let first = run(&w, Opts::default());
    assert_eq!(first.code, Some(0), "{}{}", first.stdout, first.stderr);
    let target_before = heads(&w.target);
    let dr_before = heads(&w.dr);
    let r = run(&w, Opts::default());
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(heads(&w.target), target_before);
    assert_eq!(heads(&w.dr), dr_before);
    assert_eq!(
        calls(&r.log),
        vec![format!("GET {LIST_URL}")],
        "no mirror to remove: one read, no write"
    );
    assert!(
        r.stdout
            .contains(&format!("main to {PRIVATE_DR}: up to date")),
        "{}",
        r.stdout
    );
}

/// A fetch prunes inside its own refspecs and nowhere else, so a branch a
/// past declaration fetched stays in the private clone — the live clone
/// held main that way after 67931115 — and a listing of every local head
/// read it back and reported a ref the run never pushed. The run lists
/// only what it declares.
#[test]
fn a_private_clone_left_holding_an_undeclared_branch_neither_pushes_nor_reports_it() {
    let w = world("stale-clone");
    // A tick under an older declaration carries feat/* to the fork and
    // leaves feat/forge-only in the private clone.
    let mut old = fixture_targets(&w);
    let fork = target_index(&old, FORK_CREDENTIAL);
    old[fork]["branches"] = serde_json::json!(["publish/*", "feat/*"]);
    let first = run(
        &w,
        Opts {
            targets: Some(old),
            ..Opts::default()
        },
    );
    assert_eq!(first.code, Some(0), "{}{}", first.stdout, first.stderr);
    let pushed = head_of(&w.target, "feat/forge-only").expect("the old declaration pushed it");
    // The forge's branch moves on; the next tick runs the declaration as
    // it is.
    let next = commit(&w.forge, Some(&w.b), "feature, again");
    git_in(
        &w.forge,
        &["update-ref", "refs/heads/feat/forge-only", &next],
    );
    let r = run(&w, Opts::default());
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        head_of(&w.target, "feat/forge-only"),
        Some(pushed),
        "the undeclared branch is not pushed"
    );
    let v = verdict(&r);
    assert!(
        !v.contains("feat/forge-only"),
        "the verdict names only declared refs: {v}"
    );
    assert!(
        segment_for(&v, PUBLIC_FORK).contains("read back at the forge's value: publish/2026-09-25"),
        "the verdict names exactly the declared refs it read back: {v}"
    );
}

/// Main is on the DR copy again (761bc8a9), so the 2026-09-25 shape —
/// forge main rewound — is a live case once more: refused, named, and the
/// DR copy keeps the newer main. The Forgejo mirror still goes: it was
/// never a copy of main anywhere private, only a force-push of every
/// branch to the PUBLIC fork (b176fd60 S1, 67931115).
#[test]
fn a_rewound_forge_main_is_refused_and_the_target_keeps_its_main() {
    let w = world("rewound");
    // A tick carries b off-site; then the forge's main goes BACK to a —
    // the 2026-09-25 shape, now seen from the other side.
    let first = run(&w, Opts::default());
    assert_eq!(first.code, Some(0), "{}{}", first.stdout, first.stderr);
    assert_eq!(head_of(&w.dr, "main"), Some(w.b.clone()));
    git_in(&w.forge, &["update-ref", "refs/heads/main", &w.a]);
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        head_of(&w.dr, "main"),
        Some(w.b.clone()),
        "never overwritten"
    );
    assert!(
        r.stderr.contains("refs/heads/main") && r.stderr.contains("non-fast-forward"),
        "the refused ref and why are named: {}",
        r.stderr
    );
    assert_eq!(
        r.mirrors,
        serde_json::json!([]),
        "a refusal does not keep the public-fork mirror"
    );
    let v = verdict(&r);
    assert!(
        v.starts_with("REFUSED") && segment_for(&v, PRIVATE_DR).contains("refs/heads/main"),
        "{v}"
    );
}

/// The forge holds a publish branch (fixture declaration) at a commit that
/// does not descend from the fork's — until backlog 1f0aa60d the timing of
/// a same-day re-publish; since then a real disagreement, since the verb
/// never forces. The refusal is named, exit 1, and the fork keeps what it
/// had, while main lands on the DR copy in the same run. It does not hold
/// the Forgejo mirror in place: that mirror
/// is `git push -f --mirror` of EVERY forge branch, main among them, to
/// this same public fork — the exposure itself (b176fd60 S1, 67931115).
#[test]
fn a_publish_refusal_with_main_current_still_removes_the_mirror() {
    let w = world("publish-refused");
    let first = run(&w, Opts::default());
    assert_eq!(first.code, Some(0), "{}{}", first.stdout, first.stderr);
    let published = head_of(&w.target, "publish/2026-09-25").expect("publish arrived");
    // The forge's copy moves to a commit that is NOT a descendant of the fork's.
    let republish = commit(&w.forge, Some(&w.a), "publish snapshot, again");
    git_in(
        &w.forge,
        &["update-ref", "refs/heads/publish/2026-09-25", &republish],
    );
    // ...and main moves on in the same tick, so the push that is refused
    // on one ref still lands the other.
    let c = commit(&w.forge, Some(&w.b), "c");
    git_in(&w.forge, &["update-ref", "refs/heads/main", &c]);
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert_eq!(head_of(&w.dr, "main"), Some(c), "main lands");
    assert_eq!(
        head_of(&w.target, "publish/2026-09-25"),
        Some(published),
        "the refused ref is never overwritten"
    );
    assert!(
        r.stderr.contains("refs/heads/publish/2026-09-25") && r.stderr.contains("non-fast-forward"),
        "the refusal is still named: {}",
        r.stderr
    );
    assert_eq!(
        calls(&r.log),
        vec![
            format!("GET {LIST_URL}"),
            format!("DELETE {LIST_URL}/{MIRROR_NAME}"),
            format!("GET {LIST_URL}"),
        ],
        "every target read back, so the mirror goes"
    );
    assert_eq!(r.mirrors, serde_json::json!([]));
    let v = verdict(&r);
    assert!(
        v.starts_with("REFUSED") && v.contains("publish/2026-09-25") && v.contains(MIRROR_NAME),
        "the verdict names both the refusal and the removal: {v}"
    );
    no_token_anywhere(&r);
}

/// A root unit reads /etc/gitconfig; the tests never did (they set
/// GIT_CONFIG_NOSYSTEM), so a host's `pushInsteadOf` could send the push
/// somewhere else and a `core.hooksPath` could run a hook as root, and no
/// test would have seen either (b176fd60 S3). The script sets it itself.
#[test]
fn a_system_gitconfig_cannot_redirect_the_push_or_run_a_hook() {
    let w = world("system-gitconfig");
    let hooks = w.dir.join("system-hooks");
    let marker = w.dir.join("a-system-hook-ran");
    boss_testing::create_dir(&hooks);
    boss_testing::write_exec(
        &hooks.join("pre-push"),
        &format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
    );
    let elsewhere = w.dir.join("github/somewhere-else.git");
    init_bare(&elsewhere);
    let sys = format!(
        "[url \"{}\"]\n\tpushInsteadOf = {}\n[core]\n\thooksPath = {}\n",
        elsewhere.display(),
        w.dr.display(),
        hooks.display()
    );
    let r = run(
        &w,
        Opts {
            system_gitconfig: Some(sys),
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        head_of(&w.dr, "main"),
        Some(w.b.clone()),
        "main arrives where declared"
    );
    assert!(heads(&elsewhere).is_empty(), "nothing was redirected");
    assert!(!marker.exists(), "no system hook ran");
}

/// GIT_CONFIG_COUNT / GIT_CONFIG_KEY_n / GIT_CONFIG_VALUE_n carry git
/// config past BOTH files the script pins — GIT_CONFIG_NOSYSTEM and its
/// own GIT_CONFIG_GLOBAL leave them standing — so a unit's environment
/// could redirect the push and run a hook as root (delta review of
/// bf3ee4a3). The script unsets every GIT_* variable before it sets its
/// own.
#[test]
fn git_config_from_the_environment_cannot_redirect_the_push_or_run_a_hook() {
    let w = world("env-gitconfig");
    let hooks = w.dir.join("env-hooks");
    let marker = w.dir.join("an-env-hook-ran");
    boss_testing::create_dir(&hooks);
    boss_testing::write_exec(
        &hooks.join("pre-push"),
        &format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
    );
    let elsewhere = w.dir.join("github/somewhere-else.git");
    init_bare(&elsewhere);
    let env = vec![
        ("GIT_CONFIG_COUNT".to_string(), "2".to_string()),
        (
            "GIT_CONFIG_KEY_0".to_string(),
            format!("url.{}.pushInsteadOf", elsewhere.display()),
        ),
        ("GIT_CONFIG_VALUE_0".to_string(), w.dr.display().to_string()),
        ("GIT_CONFIG_KEY_1".to_string(), "core.hooksPath".to_string()),
        (
            "GIT_CONFIG_VALUE_1".to_string(),
            hooks.display().to_string(),
        ),
    ];
    let r = run(
        &w,
        Opts {
            env,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        head_of(&w.dr, "main"),
        Some(w.b.clone()),
        "main lands where declared"
    );
    assert!(heads(&elsewhere).is_empty(), "nothing was redirected");
    assert!(!marker.exists(), "no hook from the environment ran");
}

/// BOSS_OFFSITE_GITHUB_BASE is the test seam and nothing else. Set on a
/// live unit to a remote, the push and the read-back would go THERE while
/// the verdict still named the declared GitHub repository — a record of
/// an off-site copy that was never kept (delta review of bf3ee4a3). A
/// value that is not a local directory pushes nothing — and the Forgejo
/// mirror is still removed (F1); a run made through the stand-in says so
/// in its verdict.
#[test]
fn a_github_stand_in_that_is_not_a_local_directory_is_refused() {
    for (case, base) in [
        ("remote-url", "https://github.com.evil.example".to_string()),
        ("no-such-dir", "/nonexistent/offsite-stand-in".to_string()),
    ] {
        let w = world(&format!("stand-in-{case}"));
        let fork_before = heads(&w.target);
        let r = run(
            &w,
            Opts {
                mirrors: Some(live_mirror()),
                github_base: Some(base),
                ..Opts::default()
            },
        );
        assert_eq!(r.code, Some(4), "{case}: {}{}", r.stdout, r.stderr);
        assert!(
            heads(&w.dr).is_empty(),
            "{case}: nothing reached the DR copy"
        );
        assert_eq!(heads(&w.target), fork_before, "{case}: nor the fork");
        assert!(
            git_calls(&r, "push").is_empty(),
            "{case}: no push: {}",
            r.git_log
        );
        assert_eq!(
            r.mirrors,
            serde_json::json!([]),
            "{case}: the Forgejo push mirror is still removed (F1): {}{}",
            r.stdout,
            r.stderr
        );
        let v = verdict(&r);
        assert!(
            v.starts_with("cannot answer")
                && v.contains("BOSS_OFFSITE_GITHUB_BASE")
                && v.contains("removed Forgejo push mirror"),
            "{case}: the verdict names both halves: {v}"
        );
    }
    let w = world("stand-in-named");
    let r = run(&w, Opts::default());
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    let v = verdict(&r);
    for remote in [PRIVATE_DR, PUBLIC_FORK] {
        assert!(
            segment_for(&v, remote).contains(&format!(
                "(via stand-in {})",
                w.dir.join("github").join(helper_path(remote)).display()
            )),
            "a record made through the stand-in names it: {v}"
        );
    }
}

/// A stalled GitHub used to hang the push to the unit's ten-minute
/// timeout, which kills the converge before offsite_push is written
/// (b176fd60 S4). Both network calls carry a low-speed bound, so a stall
/// is a git failure this script names.
#[test]
fn the_push_and_the_read_back_fail_fast_on_a_stall() {
    let w = world("low-speed");
    let r = run(&w, Opts::default());
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    for verb in ["push", "ls-remote"] {
        let argvs = git_calls(&r, verb);
        assert_eq!(argvs.len(), 2, "one {verb} per target: {}", r.git_log);
        for argv in &argvs {
            for key in ["http.lowSpeedLimit=", "http.lowSpeedTime="] {
                let value: u32 = argv
                    .iter()
                    .find_map(|a| a.strip_prefix(key))
                    .unwrap_or_else(|| panic!("{verb} carries no {key}: {argv:?}"))
                    .parse()
                    .expect("a number");
                assert!(value > 0, "{verb} {key}{value}");
            }
            let time: u32 = argv
                .iter()
                .find_map(|a| a.strip_prefix("http.lowSpeedTime="))
                .unwrap()
                .parse()
                .unwrap();
            assert!(time <= 120, "a stall is named in minutes, not ten: {time}s");
        }
    }
}

/// The path git hands a credential helper for `remote` when
/// credential.useHttpPath is set: the URL's path without its leading
/// slash, so https://github.com/algedonic-dev/boss-dr.git asks for
/// `algedonic-dev/boss-dr.git`.
fn helper_path(remote: &str) -> &str {
    remote
        .strip_prefix("https://github.com/")
        .unwrap_or_else(|| panic!("not a GitHub remote: {remote}"))
}

/// The helper hands the token to whatever asks. The declared remote is
/// GitHub, but a redirect, an insteadOf or a changed declaration would
/// have had it hand dauld's token to another host (b176fd60 S5). With two
/// targets it must also refuse the OTHER target's repository: the DR
/// token is scoped to boss-dr alone and the fork's token must never be
/// the one that writes main anywhere (761bc8a9). Each helper is driven
/// here exactly as the script passed it to that target's push.
#[test]
fn each_credential_helper_answers_only_its_own_repository_on_github_over_https() {
    let w = world("helper");
    let r = run(&w, Opts::default());
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    let pushes = git_calls(&r, "push");
    assert_eq!(pushes.len(), 2, "one push per target: {}", r.git_log);
    // The push goes to the seam's local stand-in; the helper is bound to
    // the DECLARED remote, which is what GitHub would be asked about.
    let dr_path = w.dr.to_str().unwrap().to_string();
    let fork_path = w.target.to_str().unwrap().to_string();
    for push in &pushes {
        assert!(
            push.contains(&"credential.useHttpPath=true"),
            "the helper is only told the path when useHttpPath is set: {push:?}"
        );
        let remote = push
            .iter()
            .skip_while(|a| **a != "--porcelain")
            .nth(1)
            .unwrap_or_else(|| panic!("the push names its remote after --porcelain: {push:?}"))
            .to_string();
        let (own, other, token, other_token) = if remote == dr_path {
            (PRIVATE_DR, PUBLIC_FORK, DR_TOKEN, GH_TOKEN)
        } else if remote == fork_path {
            (PUBLIC_FORK, PRIVATE_DR, GH_TOKEN, DR_TOKEN)
        } else {
            panic!("a push to an undeclared remote: {remote}")
        };
        let helper = push
            .iter()
            .find(|a| a.starts_with("credential.helper=!"))
            .unwrap_or_else(|| panic!("the push carries a helper: {push:?}"))
            .to_string();
        // useHttpPath as the push sets it: without it git drops `path`
        // for http(s) before any helper sees it, and a path-bound helper
        // would answer nothing at all.
        let fill = |protocol: &str, host: &str, path: &str| {
            let mut child = Command::new(real_git())
                .args([
                    "-c",
                    "credential.helper=",
                    "-c",
                    &helper,
                    "-c",
                    "credential.useHttpPath=true",
                    "credential",
                    "fill",
                ])
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("HOME", &w.dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("git runs");
            boss_testing::feed_stdin(
                &mut child,
                format!("protocol={protocol}\nhost={host}\npath={path}\n\n").as_bytes(),
            );
            let out = child.wait_with_output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let own_path = helper_path(own);
        assert!(
            fill("https", "github.com", own_path).contains(&format!("password={token}")),
            "{remote}: its own repository gets its own token"
        );
        let answer = fill("https", "github.com", helper_path(other));
        assert!(
            !answer.contains(token) && !answer.contains(other_token),
            "{remote}: the OTHER target's repository must not get this token: {answer}"
        );
        for (protocol, host) in [
            ("https", "evil.example"),
            ("https", "github.com.evil.example"),
            ("http", "github.com"),
        ] {
            assert!(
                !fill(protocol, host, own_path).contains(token),
                "{protocol}://{host} must not get the token"
            );
        }
    }
}

/// A row Forgejo lists without a remote_name is a mirror that is still
/// there — `// empty` dropped it and reported "no Forgejo push mirror"
/// (b176fd60 S5). It is a failure that says so.
#[test]
fn a_mirror_row_with_no_name_is_an_error_not_none() {
    for (case, mirrors) in [
        (
            "null-name",
            serde_json::json!([{"remote_name": null, "remote_address": "https://github.com/dauld/boss-fork.git"}]),
        ),
        (
            "no-name",
            serde_json::json!([{"remote_address": "https://github.com/dauld/boss-fork.git"}]),
        ),
    ] {
        let w = world(&format!("nameless-{case}"));
        let r = run(
            &w,
            Opts {
                mirrors: Some(mirrors.clone()),
                ..Opts::default()
            },
        );
        assert_eq!(r.code, Some(1), "{case}: {}{}", r.stdout, r.stderr);
        assert!(!r.stdout.contains("no push mirror"), "{case}: {}", r.stdout);
        let v = verdict(&r);
        assert!(
            v.starts_with("FAILED") && v.contains("remote_name"),
            "{case}: {v}"
        );
        assert_eq!(r.mirrors, mirrors, "{case}: nothing deleted");
    }
}

/// A run that cannot reach a target pushes nothing more — and STILL
/// removes the Forgejo mirror (review of fcf6042f, F1): that mirror is
/// `git push -f --mirror`, the exposure 67931115 removed and the wipe
/// 21d54f4a removed, so keeping it is never the safe side of a failed
/// push. Until that review a GitHub outage or a renamed DR repository
/// kept it in place. The exit is 1 and the verdict names both halves.
#[test]
fn a_push_that_fails_still_removes_the_forgejo_mirror() {
    let case = "dead-target";
    let w = world(case);
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            dead_target: true,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        r.mirrors,
        serde_json::json!([]),
        "{case}: the Forgejo push mirror is still removed (F1): {}{}",
        r.stdout,
        r.stderr
    );
    let v = verdict(&r);
    assert!(
        v.starts_with("FAILED") && v.contains("removed Forgejo push mirror"),
        "the verdict names the failed push and the removal: {v}"
    );
    no_token_anywhere(&r);
}

#[test]
fn a_refused_or_ignored_delete_is_a_failure() {
    let w = world("delete-403");
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            status: Some(("DELETE", "403")),
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("HTTP 403"), "{}", r.stderr);
    assert_eq!(
        head_of(&w.dr, "main"),
        Some(w.b.clone()),
        "the push still landed"
    );
    no_token_anywhere(&r);

    let w = world("delete-ignored");
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            ignore_writes: true,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("does not read back"),
        "an answer is not an effect: {}",
        r.stderr
    );
}

#[test]
fn an_unreadable_forge_api_is_exit_4_and_nothing_is_deleted() {
    let w = world("dark");
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            fail: true,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("Failed to connect"), "{}", r.stderr);
    assert_eq!(r.mirrors, live_mirror());
}

/// A 200 with nothing in it is not an empty list: on jq-1.6 `jq -e`
/// passes on no document (backlog d96e38ab), which would read as "the
/// forge carries no push mirror" while the mirror still ran.
#[test]
fn an_empty_answer_is_not_read_as_no_mirror() {
    let w = world("empty-list");
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            empty_get: true,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}{}", r.stdout, r.stderr);
    assert!(!r.stdout.contains("no push mirror"), "{}", r.stdout);
    assert!(verdict(&r).starts_with("cannot answer"), "{:?}", r.summary);
}

/// The DR slot is new, and until David mints and places its token it is
/// EMPTY. That is a refusal the packet names — the slot, the credential
/// id, what goes in it — never a tick that quietly pushes the rest and
/// reads green while no off-site copy of main is being kept (761bc8a9).
/// Nothing is pushed to EITHER target: the declaration is one unit, and
/// every precondition is checked before anything is pushed. The Forgejo
/// mirror is still removed (F1): an expired token, which the render
/// answers by removing this file, must not keep a `-f --mirror` standing.
#[test]
fn an_empty_dr_credential_slot_is_a_refusal_not_a_skip() {
    for (case, opts) in [
        (
            "dr-slot-absent",
            Opts {
                no_token: Some(DR_CREDENTIAL),
                ..Opts::default()
            },
        ),
        (
            "dr-slot-empty",
            Opts {
                empty_token: Some(DR_CREDENTIAL),
                ..Opts::default()
            },
        ),
    ] {
        let w = world(case);
        let fork_before = heads(&w.target);
        let r = run(
            &w,
            Opts {
                mirrors: Some(live_mirror()),
                ..opts
            },
        );
        assert_eq!(r.code, Some(4), "{case}: {}{}", r.stdout, r.stderr);
        assert!(
            heads(&w.dr).is_empty(),
            "{case}: nothing reached the DR copy"
        );
        assert_eq!(heads(&w.target), fork_before, "{case}: nor the fork");
        assert_eq!(
            r.mirrors,
            serde_json::json!([]),
            "{case}: the Forgejo push mirror is still removed (F1): {}{}",
            r.stdout,
            r.stderr
        );
        let v = verdict(&r);
        let slot = w.dir.join("github-dr.token");
        assert!(
            v.starts_with("cannot answer")
                && v.contains(DR_CREDENTIAL)
                && v.contains(slot.to_str().unwrap())
                && v.contains("refusal, not a skip")
                && v.contains("removed Forgejo push mirror"),
            "{case}: the verdict names the slot, its credential, that it refuses, and the removal: {v}"
        );
        no_token_anywhere(&r);
    }
}

#[test]
fn a_missing_or_loose_credential_is_exit_4_and_pushes_nothing() {
    for (case, opts) in [
        (
            "no-fork-token",
            Opts {
                no_token: Some(FORK_CREDENTIAL),
                ..Opts::default()
            },
        ),
        (
            "loose-gh-token",
            Opts {
                token_mode: Some(0o644),
                ..Opts::default()
            },
        ),
        (
            "no-forge-auth",
            Opts {
                no_forge_auth: true,
                ..Opts::default()
            },
        ),
        // 0600 says who may read it; the owner says who could have written
        // it. A token file some other account owns can be swapped for a
        // token of that account's choosing (backlog b176fd60 S5).
        (
            "foreign-owned-gh-token",
            Opts {
                foreign_token_owner: true,
                ..Opts::default()
            },
        ),
    ] {
        let w = world(case);
        let target_before = heads(&w.target);
        let r = run(
            &w,
            Opts {
                mirrors: Some(live_mirror()),
                ..opts
            },
        );
        assert_eq!(r.code, Some(4), "{case}: {}{}", r.stdout, r.stderr);
        assert_eq!(heads(&w.target), target_before, "{case}: nothing pushed");
        assert!(
            heads(&w.dr).is_empty(),
            "{case}: nothing pushed to the DR copy"
        );
        if case == "no-forge-auth" {
            // The mirror half's own input: without the header nothing
            // can be listed or removed, so nothing is touched.
            assert!(calls(&r.log).is_empty(), "{case}: {:?}", calls(&r.log));
            assert_eq!(r.mirrors, live_mirror(), "{case}");
        } else {
            assert_eq!(
                r.mirrors,
                serde_json::json!([]),
                "{case}: the Forgejo push mirror is still removed (F1): {}{}",
                r.stdout,
                r.stderr
            );
        }
        assert!(
            verdict(&r).starts_with("cannot answer"),
            "{case}: {:?}",
            r.summary
        );
        no_token_anywhere(&r);
    }
}

#[test]
fn a_declaration_that_could_force_or_rename_is_refused() {
    for (case, branches) in [
        ("force", serde_json::json!(["+publish/*"])),
        ("rename", serde_json::json!(["publish/x:other"])),
        ("not-a-list", serde_json::json!("publish/*")),
        ("glob-mid", serde_json::json!(["pub*/x"])),
    ] {
        let w = world(&format!("decl-{case}"));
        let target_before = heads(&w.target);
        let mut targets = fixture_targets(&w);
        let fork = target_index(&targets, FORK_CREDENTIAL);
        targets[fork]["branches"] = branches;
        let r = run(
            &w,
            Opts {
                targets: Some(targets),
                ..Opts::default()
            },
        );
        assert_eq!(r.code, Some(2), "{case}: {}{}", r.stdout, r.stderr);
        assert_eq!(heads(&w.target), target_before, "{case}");
        assert!(heads(&w.dr).is_empty(), "{case}");
    }
}

/// Main has exactly one off-site home, the private DR copy, and rides
/// there alone. Each shape below is a one-edit way back to main on the
/// public fork — or to no off-site main at all — and each is refused
/// before anything is written, naming why (761bc8a9). The pre-2026-09-27
/// declaration, one target carrying main AND publish/*, is the first.
#[test]
fn a_declaration_that_could_put_main_on_the_fork_or_share_a_slot_is_refused() {
    let w0 = world("decl-shapes");
    let base = fixture_targets(&w0);
    let dr = target_index(&base, DR_CREDENTIAL);
    let fork = target_index(&base, FORK_CREDENTIAL);
    let mut cases: Vec<(&str, serde_json::Value, &str)> = Vec::new();

    let mut old_shape = base[fork].clone();
    old_shape["branches"] = serde_json::json!(["main", "publish/*"]);
    cases.push((
        "the-old-single-target",
        serde_json::json!([old_shape]),
        "nowhere else",
    ));

    let mut t = base.clone();
    t[fork]["branches"] = serde_json::json!(["main", "publish/*"]);
    cases.push(("main-on-the-fork-too", t, "exactly one target"));

    let mut t = base.clone();
    t[dr]["branches"] = serde_json::json!(["main", "publish/*"]);
    cases.push(("publish-on-the-dr-copy", t, "main rides alone"));

    let mut t = base.clone();
    t[dr]["branches"] = serde_json::json!(["release/*"]);
    cases.push(("no-main-anywhere", t, "exactly one target"));

    let mut t = base.clone();
    t[dr]["token_file"] = base[fork]["token_file"].clone();
    cases.push(("one-slot-for-both", t, "its own credential slot"));

    let mut t = base.clone();
    t[fork]["remote"] = base[dr]["remote"].clone();
    cases.push(("one-remote-for-both", t, "share a remote"));

    // Two spellings of one repository are one repository: GitHub answers
    // them alike, whatever the case or the .git suffix.
    let mut t = base.clone();
    t[fork]["remote"] = serde_json::json!("https://github.com/Algedonic-Dev/Boss-DR");
    cases.push(("one-remote-spelled-twice", t, "share a remote"));

    // And two spellings of one path are one file.
    let mut t = base.clone();
    let doubled = base[fork]["token_file"]
        .as_str()
        .unwrap()
        .replace("/github.token", "//github.token");
    t[dr]["token_file"] = serde_json::json!(doubled);
    cases.push(("one-slot-spelled-twice", t, "its own credential slot"));

    // A remote is a GitHub repository over https, owner/name, and nothing
    // else: no userinfo, no other host, no local path, no port.
    for (case, remote) in [
        (
            "remote-userinfo",
            "https://x@github.com/algedonic-dev/boss-dr.git",
        ),
        (
            "remote-other-host",
            "https://github.com.evil.example/a/boss-dr.git",
        ),
        ("remote-local-path", "/srv/git/boss-dr.git"),
        (
            "remote-port",
            "https://github.com:443/algedonic-dev/boss-dr.git",
        ),
        (
            "remote-deeper-path",
            "https://github.com/algedonic-dev/boss-dr/x.git",
        ),
    ] {
        let mut t = base.clone();
        t[dr]["remote"] = serde_json::json!(remote);
        cases.push((case, t, "naming a remote"));
    }

    // Main declared ONLY on the fork: one target, alone — the script
    // itself refuses any remote but the DR copy for main (F2).
    let mut t = base.clone();
    t[dr]["branches"] = serde_json::json!([]);
    t[fork]["branches"] = serde_json::json!(["main"]);
    cases.push(("main-only-on-the-fork", t, "nowhere else"));

    // A trailing newline passed jq 1.6's `$` (review of fcf6042f, F2):
    // "main\n" and "/etc/x\n" read as valid, and a newline in one field
    // shifted the per-field lists against each other.
    for (case, field, value) in [
        (
            "newline-remote",
            "remote",
            serde_json::json!("https://github.com/algedonic-dev/boss-dr.git\n"),
        ),
        ("newline-branch", "branches", serde_json::json!(["main\n"])),
        (
            "newline-token-file",
            "token_file",
            serde_json::json!(format!("{}\n", base[dr]["token_file"].as_str().unwrap())),
        ),
        (
            "newline-credential",
            "credential",
            serde_json::json!("github-dr-push-token\n"),
        ),
    ] {
        let mut t = base.clone();
        t[dr][field] = value;
        cases.push((case, t, "naming a remote"));
    }
    // The shape the review worked out: the fork's token file carrying a
    // trailing newline, so four separate lists would pair the fork's
    // remote and token with main.
    let mut t = base.clone();
    t[fork]["token_file"] =
        serde_json::json!(format!("{}\n", base[fork]["token_file"].as_str().unwrap()));
    cases.push(("newline-shifts-the-lists", t, "naming a remote"));

    for (case, targets, why) in cases {
        let w = world(&format!("decl-{case}"));
        // The fixture paths above were aimed at w0; re-aim them at w.
        let text = targets
            .to_string()
            .replace(w0.dir.to_str().unwrap(), w.dir.to_str().unwrap());
        let targets: serde_json::Value = serde_json::from_str(&text).unwrap();
        let fork_before = heads(&w.target);
        let r = run(
            &w,
            Opts {
                mirrors: Some(live_mirror()),
                targets: Some(targets),
                ..Opts::default()
            },
        );
        assert_eq!(r.code, Some(2), "{case}: {}{}", r.stdout, r.stderr);
        assert_eq!(
            r.mirrors,
            serde_json::json!([]),
            "{case}: the Forgejo push mirror is still removed (F1): {}{}",
            r.stdout,
            r.stderr
        );
        assert_eq!(heads(&w.target), fork_before, "{case}: the fork untouched");
        assert!(heads(&w.dr).is_empty(), "{case}: the DR copy untouched");
        assert!(
            r.stderr.contains(why),
            "{case}: names why ({why}): {}",
            r.stderr
        );
        assert!(
            verdict(&r).starts_with("REFUSED:"),
            "{case}: {:?}",
            r.summary
        );
    }
}

/// A BROKEN DECLARATION STILL REMOVES THE MIRROR (the review of
/// 590d3384). The declaration refusal used to `exit 2` before anything
/// else ran, so a tick whose declaration was malformed left a re-created
/// Forgejo push mirror standing — and that mirror force-pushes EVERY
/// forge branch, main among them, to the public fork. The refusal now
/// pushes nothing, removes the mirror, and still exits 2, with a verdict
/// that begins with the refusal (never with the success words a probe
/// reads) and names the removal.
#[test]
fn a_broken_declaration_pushes_nothing_and_still_removes_the_mirror() {
    let w0 = world("broken-shapes");
    let base = fixture_targets(&w0);
    let fork = target_index(&base, FORK_CREDENTIAL);
    let dr = target_index(&base, DR_CREDENTIAL);
    let mut force = base.clone();
    force[dr]["branches"] = serde_json::json!(["+main"]);
    let mut not_a_list = base.clone();
    not_a_list[fork]["branches"] = serde_json::json!("publish/*");
    let mut main_twice = base.clone();
    main_twice[fork]["branches"] = serde_json::json!(["main"]);
    for (case, targets) in [
        ("force", force),
        ("not-a-list", not_a_list),
        ("main-twice", main_twice),
    ] {
        let w = world(&format!("broken-{case}"));
        let text = targets
            .to_string()
            .replace(w0.dir.to_str().unwrap(), w.dir.to_str().unwrap());
        let targets: serde_json::Value = serde_json::from_str(&text).unwrap();
        let target_before = heads(&w.target);
        let r = run(
            &w,
            Opts {
                mirrors: Some(live_mirror()),
                targets: Some(targets),
                ..Opts::default()
            },
        );
        assert_eq!(r.code, Some(2), "{case}: {}{}", r.stdout, r.stderr);
        assert_eq!(heads(&w.target), target_before, "{case}");
        assert!(heads(&w.dr).is_empty(), "{case}");
        assert!(
            git_calls(&r, "push").is_empty() && git_calls(&r, "fetch").is_empty(),
            "{case}: a refused declaration neither fetches nor pushes: {}",
            r.git_log
        );
        assert_eq!(
            r.mirrors,
            serde_json::json!([]),
            "{case}: the Forgejo push mirror is removed even when the declaration is refused: {}{}",
            r.stdout,
            r.stderr
        );
        let v = verdict(&r);
        assert!(
            v.starts_with("REFUSED:") && v.contains("removed Forgejo push mirror"),
            "{case}: the verdict begins with the refusal and names the removal: {v}"
        );
        assert!(
            !v.contains("read back at the forge's value") && !v.contains("nothing declared for"),
            "{case}: a refused declaration never carries the success words: {v}"
        );
        no_token_anywhere(&r);
    }
}

/// NO SILENT EXIT (review of fcf6042f, F2). A stop the script did not
/// plan — here sor.sh refusing an unset BOSS_FORGE_URL, the same path a
/// `set -u` abort takes — used to leave the packet with no offsite_push at
/// all. The EXIT trap writes one that begins REFUSED: and names the exit.
#[test]
fn a_run_that_stops_without_a_verdict_still_writes_one() {
    let w = world("no-verdict");
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            no_forge_url: true,
            ..Opts::default()
        },
    );
    assert_ne!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("BOSS_FORGE_URL"),
        "sor.sh says why: {}",
        r.stderr
    );
    let v = verdict(&r);
    assert!(
        v.starts_with("REFUSED:") && v.contains("before it wrote a verdict"),
        "the trap writes the verdict the run did not: {v}"
    );
    assert!(heads(&w.dr).is_empty(), "nothing pushed");
    no_token_anywhere(&r);
}

/// THE FORK PIN (backlog a2b58aab, the adversarial review of 67931115's
/// first car; before it, 67931115 / design 1f35a3e8). dauld/boss-mirror is
/// a fork of the public algedonic-dev/boss, and a fork of a public
/// repository is public — whatever reaches it is published. Forge main
/// left it on 2026-09-27 because it is every train, unsigned and never
/// secrets-scanned. `publish/*` stayed, on the reading that the publish
/// flow had scanned and signed it — but a forge BRANCH NAME vouches for
/// nothing: every holder of a forge write credential for user david can
/// push refs/heads/publish/<anything>, and the forge protects main alone,
/// so the carry put any such branch on the public fork within one tick.
/// And the carry was never needed: publish-github-pr.sh pushes each
/// snapshot to the fork ITSELF, after its scan and the passkey check
/// (`a_real_fork_of_the_mirror_is_published_to` in publish_github_pr_sh.rs).
/// So the converge carries NOTHING to the public fork. Any branch on a
/// target aimed at it — publish/* included — is refused at the gate.
#[test]
fn the_public_fork_receives_nothing_from_the_converge() {
    let d = declaration();
    let targets = d["targets"]
        .as_array()
        .expect("offsite-push.json declares a targets list");
    let fork: Vec<&serde_json::Value> = targets
        .iter()
        .filter(|t| t["remote"] == PUBLIC_FORK)
        .collect();
    assert!(!fork.is_empty(), "the fork is declared, with nothing: {d}");
    for t in fork {
        let branches = t["branches"]
            .as_array()
            .unwrap_or_else(|| panic!("a target with no branches list: {t}"));
        assert!(
            branches.is_empty(),
            "offsite-push.json carries {branches:?} to {PUBLIC_FORK} — a PUBLIC fork. The \
             fork receives publish branches only from publish-github-pr.sh, which scans the \
             approved tree and checks its passkey sign-off; a forge branch name vouches for \
             nothing, since every forge write credential can create one (backlog a2b58aab)"
        );
    }
}

/// THE DR PIN (backlog 761bc8a9; design 76155676, David 2026-09-27). The
/// private DR copy carries main and only main, and no remote other than
/// it and the fork is declared at all — a new target is a decision about
/// which branches may leave the forge, and to where, and this pin is where
/// it is recorded.
#[test]
fn the_dr_copy_is_declared_main_only() {
    let d = declaration();
    let targets = d["targets"]
        .as_array()
        .expect("offsite-push.json declares a targets list");
    assert!(!targets.is_empty(), "offsite-push.json declares no target");
    for t in targets {
        let branches: Vec<&str> = t["branches"]
            .as_array()
            .unwrap_or_else(|| panic!("a target with no branches list: {t}"))
            .iter()
            .map(|b| b.as_str().unwrap_or_default())
            .collect();
        match t["remote"].as_str() {
            Some(PUBLIC_FORK) => {}
            Some(PRIVATE_DR) => assert_eq!(
                branches,
                vec!["main"],
                "the private DR copy {PRIVATE_DR} carries main and only main (backlog 761bc8a9)"
            ),
            other => panic!(
                "offsite-push.json declares a target this pin has no decision for: {other:?} — \
                 which branches may leave the forge, and to where, is decided here"
            ),
        }
    }
}

/// The real declaration, run: main reaches the DR copy, nothing is pushed
/// to the fork — so the fork's token is not needed, and is absent here: a
/// converge must not fail for a credential it does not use. The Forgejo
/// push mirror is still removed (it would force-push every forge branch,
/// main among them, to the fork), and the verdict names both targets: main
/// read back on the DR copy — the words the car's recorded probe reads —
/// and the fork receiving nothing from here. An empty list must NEVER
/// reach `git push`: with no refspec git falls back to push.default, which
/// is not a declaration.
#[test]
fn the_real_declaration_pushes_main_to_the_dr_copy_and_nothing_to_the_fork() {
    let w = world("real-declaration");
    let fork_before = heads(&w.target);
    let forge_before = heads(&w.forge);
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            real_declaration: true,
            no_token: Some(FORK_CREDENTIAL),
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        head_of(&w.dr, "main"),
        Some(w.b.clone()),
        "main on the DR copy"
    );
    assert_eq!(heads(&w.target), fork_before, "the fork is untouched");
    assert_eq!(heads(&w.forge), forge_before, "the forge is untouched");
    let pushes = git_calls(&r, "push");
    let fork_path = w.target.to_str().unwrap();
    assert_eq!(pushes.len(), 1, "one push, the DR copy's: {}", r.git_log);
    assert!(
        !pushes[0].contains(&fork_path),
        "no push is aimed at the fork: {}",
        r.git_log
    );
    assert_eq!(
        r.mirrors,
        serde_json::json!([]),
        "the Forgejo push mirror is still removed: {}{}",
        r.stdout,
        r.stderr
    );
    let v = verdict(&r);
    assert!(
        segment_for(&v, PRIVATE_DR).starts_with(&format!(
            "main to {PRIVATE_DR}: pushed main; read back at the forge's value: main"
        )),
        "the DR part begins with the words the car's probe reads: {v}"
    );
    let fork = segment_for(&v, PUBLIC_FORK);
    assert!(
        fork.starts_with(&format!("nothing declared for {PUBLIC_FORK}"))
            && fork.contains("publish-github-pr.sh"),
        "the fork's part says it receives nothing from here, and from what it does: {v}"
    );
    assert!(!names_main(fork), "the fork's part names no main: {v}");
    assert!(
        v.ends_with("— read back: none left"),
        "the mirror half's good outcome closes a success verdict: {v}"
    );
    no_token_anywhere(&r);
}

/// The declaration is the decided one (761bc8a9; design 76155676, David
/// 2026-09-27: the DR copy is algedonic-dev/boss-dr, org-owned, private,
/// not a fork): the DR copy and the fork, each read with its own
/// credential slot. The fork is named by its canonical URL,
/// dauld/boss-mirror, rather than the boss-fork name GitHub 301-redirects
/// there: git follows a redirect only on a connection's first request
/// (http.followRedirects defaults to `initial`), and the credential
/// helper is bound to the path it was declared with, so a renamed path
/// would answer with no token at all. It is the same fork the publish
/// verb opens its PRs from, read with the same token file — two spellings
/// of each fact, pinned (CLAUDE.md §9a). Which branches each may carry is
/// the pin above.
#[test]
fn the_declaration_is_the_decided_one_and_matches_the_publish_fork() {
    let d = declaration();
    let targets = d["targets"].as_array().expect("a targets list");
    assert_eq!(targets.len(), 2, "the DR copy and the fork: {d}");
    let dr = &targets[target_index(&d["targets"], DR_CREDENTIAL)];
    let fork = &targets[target_index(&d["targets"], FORK_CREDENTIAL)];

    assert_eq!(dr["remote"], serde_json::json!(PRIVATE_DR));
    assert_eq!(
        dr["token_file"],
        serde_json::json!("/etc/boss-publish/github-dr.token")
    );
    assert_eq!(fork["remote"], serde_json::json!(PUBLIC_FORK));
    assert_ne!(
        dr["token_file"], fork["token_file"],
        "the DR copy has its own credential slot"
    );

    let publish = std::fs::read_to_string(repo_root().join("infra/forge/publish-github-pr.sh"))
        .expect("publish-github-pr.sh");
    assert!(
        publish.contains(r#"FORK_SLUG="${BOSS_FORK_SLUG:-dauld/boss-mirror}""#)
            && publish.contains(r#"https://github.com/${FORK_SLUG}.git"#),
        "the publish verb's fork and the off-site push's fork must be one repository"
    );
    let fork_token = fork["token_file"].as_str().unwrap();
    assert!(
        publish.contains(&format!(
            r#"TOKEN_FILE="${{BOSS_GITHUB_TOKEN_FILE:-{fork_token}}}""#
        )),
        "the fork is written with the publish verb's token file, {fork_token}"
    );
}

/// No line of the script that runs a push carries a way to overwrite or
/// delete on the target.
#[test]
fn the_script_never_forces_mirrors_or_prunes() {
    let text = std::fs::read_to_string(script()).expect("offsite-push.sh");
    for (n, line) in text.lines().enumerate() {
        let code = line.split('#').next().unwrap_or_default();
        if !code.contains(" push ") {
            continue;
        }
        for bad in [
            "--force", " -f ", "--mirror", "--prune", "--delete", "'+refs", "\"+refs",
        ] {
            assert!(
                !code.contains(bad),
                "offsite-push.sh:{}: a push carrying {bad}: {line}",
                n + 1
            );
        }
    }
}

#[test]
fn the_forge_converge_runs_it_after_protecting_main() {
    let converge = std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.sh"))
        .expect("forge-converge.sh");
    let protect = converge
        .find(r#""$REPO/infra/forge/protect-main.sh""#)
        .expect("protect-main runs");
    let offsite = converge
        .find(r#""$REPO/infra/forge/offsite-push.sh""#)
        .expect("forge-converge.sh must run offsite-push.sh on every tick");
    assert!(offsite > protect, "after protect-main");
    assert!(
        converge.contains("offsite_rc"),
        "its verdict decides the converge's exit"
    );
}
