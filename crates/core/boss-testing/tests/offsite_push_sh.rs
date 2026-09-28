//! `infra/forge/offsite-push.sh` — the forge converge pushes `publish/*`,
//! and ONLY `publish/*`, to dauld/boss-mirror on GitHub with a PLAIN push
//! — no force, no `--mirror`, no prune — reads the push back, and only
//! then removes Forgejo's own push mirror (backlog 21d54f4a, decided
//! 2026-09-26 under David's rule that no mirror can wipe what it mirrors).
//!
//! Forge `main` left the list on 2026-09-27 (backlog 67931115, design
//! 1f35a3e8): dauld/boss-mirror is a fork of the public algedonic-dev/boss,
//! a fork of a public repository is public and cannot be made private,
//! and so every train reached public GitHub within one converge tick —
//! unsigned, and outside the publish flow's secrets scan and passkey.
//! `publish/*` is what that flow has already scanned and signed. The
//! declaration's pin is `the_public_fork_is_declared_publish_branches_only`.
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
//!   * publish/* reaches the target; forge main and every other branch
//!     outside the declaration do not, and a branch only the TARGET holds
//!     survives (no prune), whether or not it matches the declaration
//!   * a private clone still holding main from the old declaration
//!     neither pushes it nor reports it: the verdict names what it pushed
//!   * the forge's repository is only READ: its refs and config are the
//!     same after a run — this is the writer that rewound main, gone
//!   * a non-fast-forward publish/<date> is REFUSED, named, exit 1, and
//!     the target keeps what it had — never overwritten
//!   * the Forgejo mirror is deleted once the push has been read back,
//!     refusals or not; a push that fails stops before it; a refused or
//!     ignored delete is exit 1
//!   * no GitHub token, a world-readable one, one root does not own, or
//!     no forge credential: exit 4 before anything is pushed or deleted
//!   * a system gitconfig cannot redirect the push or run a hook; the
//!     push and read-back carry a low-speed bound; the credential helper
//!     answers only https://github.com; a nameless mirror row is an error
//!   * the declaration names the canonical dauld/boss-mirror (the fork the
//!     publish verb opens its PRs from), declares publish/* branches only,
//!     and carries no force refspec
//!   * forge-converge.sh runs it after protect-main.sh, and its verdict
//!     reaches the packet

use boss_testing::repo_root;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const GH_TOKEN: &str = "ghp-offsite-token-must-never-print-0123456789";
const FJ_TOKEN: &str = "fj-token-must-never-print-9876543210";
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
    target: PathBuf,
    state: PathBuf,
    /// forge main's two commits: `a` then `b` (b's parent is a).
    a: String,
    b: String,
    publish: String,
}

/// The forge holds main at `b`, publish/2026-09-25, and a feature branch.
/// The target (GitHub) holds main at `a`, a branch of its own, and a
/// publish branch the forge no longer has.
fn world(case: &str) -> World {
    let dir = boss_testing::scratch_dir(&format!("offsite-push-{case}"));
    let forge = dir.join("forge/david/boss.git");
    let target = dir.join("github/boss-mirror.git");
    std::fs::create_dir_all(forge.parent().unwrap()).unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    init_bare(&forge);
    init_bare(&target);
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
        state,
        a,
        b,
        publish,
    }
}

#[derive(Default)]
struct Opts {
    mirrors: Option<serde_json::Value>,
    fail: bool,
    status: Option<(&'static str, &'static str)>,
    ignore_writes: bool,
    no_gh_token: bool,
    gh_token_mode: Option<u32>,
    no_forge_auth: bool,
    /// Replaces the declaration's `branches`.
    branches: Option<serde_json::Value>,
    /// Points the push at a path that is not a repository.
    dead_target: bool,
    /// The forge answers the mirror list 200 with an empty body.
    empty_get: bool,
    /// A system gitconfig (its body) the run can see: GIT_CONFIG_SYSTEM
    /// names it and GIT_CONFIG_NOSYSTEM is NOT set, the way a host's
    /// /etc/gitconfig reaches a root unit.
    system_gitconfig: Option<String>,
    /// The token file is owned by an account other than the one the
    /// script is told must own it (root on the forge host).
    foreign_token_owner: bool,
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
/// `$GIT_LOG` and hands on to the real one — so a test can read what the
/// push and the read-back were CALLED with, not what the script's text
/// happens to say.
fn write_git_wrapper(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    boss_testing::write_exec(
        &bin.join("git"),
        &format!(
            "#!/usr/bin/env bash\n{{ printf '%s\\n' \"$@\"; echo '=== git ==='; }} >> \"$GIT_LOG\"\nexec '{}' \"$@\"\n",
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
    // The declaration under test: the real one's shape, aimed at the
    // fixture target.
    let remote = if o.dead_target {
        w.dir.join("github/no-such-repo.git")
    } else {
        w.target.clone()
    };
    let mut decl = declaration();
    decl["remote"] = serde_json::json!(remote.to_str().unwrap());
    if let Some(b) = &o.branches {
        decl["branches"] = b.clone();
    }
    let decl_path = w.dir.join("offsite-push.json");
    boss_testing::write_file(&decl_path, &decl.to_string());

    let gh_token = w.dir.join("github.token");
    if !o.no_gh_token {
        boss_testing::write_file(&gh_token, &format!("{GH_TOKEN}\n"));
        std::fs::set_permissions(
            &gh_token,
            std::fs::Permissions::from_mode(o.gh_token_mode.unwrap_or(0o600)),
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
        .env("GIT_LOG", &git_log)
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
    let out = cmd
        .env("BOSS_OFFSITE_PUSH_DECL", &decl_path)
        .env("BOSS_OFFSITE_STATE_DIR", w.dir.join("offsite-state"))
        .env("BOSS_OFFSITE_CURL", &curl)
        .env("BOSS_GITHUB_TOKEN_FILE", &gh_token)
        .env("BOSS_FORGE_REPO_PATH", &w.forge)
        .env("BOSS_FORGE_URL", "http://forge.test:3000")
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
    text.split(|c: char| c.is_whitespace() || ",;:()".contains(c))
        .any(|t| t == "main" || t == "refs/heads/main")
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
            !text.contains(FJ_TOKEN),
            "the forge token reached {what}:\n{text}"
        );
    }
}

/// The target is a public fork, so forge main — every train, unsigned and
/// unscanned — must not reach it; publish/*, which the publish flow has
/// already scanned and signed, does (backlog 67931115).
#[test]
fn publish_reaches_the_public_fork_and_forge_main_never_does() {
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
        head_of(&w.target, "main"),
        Some(w.a.clone()),
        "forge main (b) is not pushed: the target's main stays where it was"
    );
    assert_eq!(
        head_of(&w.target, "publish/2026-09-25"),
        Some(w.publish.clone()),
        "publish/* arrives"
    );
    let v = verdict(&r);
    assert!(
        v.contains("publish/*") && v.contains("publish/2026-09-25"),
        "the verdict names the declaration and the ref it pushed: {v}"
    );
    assert!(!names_main(&v), "the verdict names no main: {v}");
    assert!(
        git_calls(&r, "push")
            .iter()
            .flatten()
            .all(|a| !a.contains("refs/heads/main")),
        "no push names main: {}",
        r.git_log
    );
    assert_eq!(
        head_of(&w.target, "feat/forge-only"),
        None,
        "a branch outside the declaration is not pushed"
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
    let r = run(&w, Opts::default());
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(heads(&w.target), target_before);
    assert_eq!(
        calls(&r.log),
        vec![format!("GET {LIST_URL}")],
        "no mirror to remove: one read, no write"
    );
    assert!(r.stdout.contains("up to date"), "{}", r.stdout);
}

/// The live private clone (/var/lib/boss-offsite/boss.git) fetched main
/// under the old declaration, and a fetch of publish/* alone neither
/// updates nor prunes it. So the clone still holds a main after the
/// declaration drops it — and a listing of every local head read it back
/// against the fork's old main and counted it, a verdict about a ref this
/// run never pushed (backlog 67931115). The run lists only what it
/// declares.
#[test]
fn a_private_clone_left_holding_main_neither_pushes_nor_reports_it() {
    let w = world("stale-clone");
    // A tick under the old declaration carries b off-site and leaves main
    // in the private clone.
    let old = run(
        &w,
        Opts {
            branches: Some(serde_json::json!(["main", "publish/*"])),
            ..Opts::default()
        },
    );
    assert_eq!(old.code, Some(0), "{}{}", old.stdout, old.stderr);
    assert_eq!(head_of(&w.target, "main"), Some(w.b.clone()));
    // Forge main moves on; the next tick runs the declaration as it is.
    let c = commit(&w.forge, Some(&w.b), "c");
    git_in(&w.forge, &["update-ref", "refs/heads/main", &c]);
    let r = run(&w, Opts::default());
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        head_of(&w.target, "main"),
        Some(w.b.clone()),
        "the fork's main is left where the old declaration put it"
    );
    let v = verdict(&r);
    assert!(!names_main(&v), "the verdict names no main: {v}");
    assert!(
        v.contains("read back at the forge's value: publish/2026-09-25"),
        "the verdict names exactly the declared refs it read back: {v}"
    );
}

/// A same-day re-publish force-moves publish/<date> on the forge, and a
/// tick in the seconds before the verb's own fork push finds that ref
/// non-fast-forward. The refusal is named, exit 1, and the fork keeps
/// what it had. It does not hold the Forgejo mirror in place: that mirror
/// is `git push -f --mirror` of EVERY forge branch, main among them, to
/// this same public fork — the exposure itself (b176fd60 S1, 67931115).
#[test]
fn a_publish_refusal_is_named_and_still_removes_the_mirror() {
    let w = world("publish-refused");
    let first = run(&w, Opts::default());
    assert_eq!(first.code, Some(0), "{}{}", first.stdout, first.stderr);
    let published = head_of(&w.target, "publish/2026-09-25").expect("publish arrived");
    // The verb re-publishes: a new snapshot, NOT a descendant of the old.
    let republish = commit(&w.forge, Some(&w.a), "publish snapshot, again");
    git_in(
        &w.forge,
        &["update-ref", "refs/heads/publish/2026-09-25", &republish],
    );
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
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
        "the push read back, so the mirror goes"
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
        w.target.display(),
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
        head_of(&w.target, "publish/2026-09-25"),
        Some(w.publish.clone()),
        "publish/* arrives where declared"
    );
    assert!(heads(&elsewhere).is_empty(), "nothing was redirected");
    assert!(!marker.exists(), "no system hook ran");
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
        assert_eq!(argvs.len(), 1, "one {verb}: {}", r.git_log);
        for key in ["http.lowSpeedLimit=", "http.lowSpeedTime="] {
            let value: u32 = argvs[0]
                .iter()
                .find_map(|a| a.strip_prefix(key))
                .unwrap_or_else(|| panic!("{verb} carries no {key}: {:?}", argvs[0]))
                .parse()
                .expect("a number");
            assert!(value > 0, "{verb} {key}{value}");
        }
        let time: u32 = argvs[0]
            .iter()
            .find_map(|a| a.strip_prefix("http.lowSpeedTime="))
            .unwrap()
            .parse()
            .unwrap();
        assert!(time <= 120, "a stall is named in minutes, not ten: {time}s");
    }
}

/// The helper hands the token to whatever asks. The declared remote is
/// GitHub, but a redirect, an insteadOf or a changed declaration would
/// have had it hand dauld's token to another host (b176fd60 S5). It is
/// driven here exactly as the script passed it to the push.
#[test]
fn the_credential_helper_answers_only_github_over_https() {
    let w = world("helper");
    let r = run(&w, Opts::default());
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    let push = git_calls(&r, "push");
    let helper = push[0]
        .iter()
        .find(|a| a.starts_with("credential.helper=!"))
        .unwrap_or_else(|| panic!("the push carries a helper: {:?}", push[0]))
        .to_string();
    let fill = |protocol: &str, host: &str| {
        let mut child = Command::new(real_git())
            .args([
                "-c",
                "credential.helper=",
                "-c",
                &helper,
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
            format!("protocol={protocol}\nhost={host}\npath=dauld/boss-mirror.git\n\n").as_bytes(),
        );
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert!(
        fill("https", "github.com").contains(&format!("password={GH_TOKEN}")),
        "GitHub gets the token"
    );
    for (protocol, host) in [
        ("https", "evil.example"),
        ("https", "github.com.evil.example"),
        ("http", "github.com"),
    ] {
        assert!(
            !fill(protocol, host).contains(GH_TOKEN),
            "{protocol}://{host} must not get the token"
        );
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

/// A run that cannot reach the target stops there and changes nothing
/// else; the next tick asks again.
#[test]
fn a_push_that_fails_leaves_the_forgejo_mirror_in_place() {
    let w = world("dead-target");
    let r = run(
        &w,
        Opts {
            mirrors: Some(live_mirror()),
            dead_target: true,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert!(calls(&r.log).iter().all(|c| !c.starts_with("DELETE")));
    assert_eq!(r.mirrors, live_mirror(), "a failed run writes nothing more");
    assert!(verdict(&r).starts_with("FAILED"), "{:?}", r.summary);
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
        head_of(&w.target, "publish/2026-09-25"),
        Some(w.publish.clone()),
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

#[test]
fn a_missing_or_loose_credential_is_exit_4_before_anything_is_written() {
    for (case, opts) in [
        (
            "no-gh-token",
            Opts {
                no_gh_token: true,
                ..Opts::default()
            },
        ),
        (
            "loose-gh-token",
            Opts {
                gh_token_mode: Some(0o644),
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
        assert!(calls(&r.log).is_empty(), "{case}: {:?}", calls(&r.log));
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
        ("force", serde_json::json!(["+main"])),
        ("rename", serde_json::json!(["main:other"])),
        ("empty", serde_json::json!([])),
        ("glob-mid", serde_json::json!(["pub*/x"])),
    ] {
        let w = world(&format!("decl-{case}"));
        let target_before = heads(&w.target);
        let r = run(
            &w,
            Opts {
                branches: Some(branches),
                ..Opts::default()
            },
        );
        assert_eq!(r.code, Some(2), "{case}: {}{}", r.stdout, r.stderr);
        assert_eq!(heads(&w.target), target_before, "{case}");
    }
}

/// THE PIN (backlog 67931115, design 1f35a3e8, decided by David
/// 2026-09-27). dauld/boss-mirror is a fork of the public
/// algedonic-dev/boss, and a fork of a public repository is public — it
/// cannot be made private. Whatever this file pushes there is published.
/// Forge main is every train, unsigned and never secrets-scanned; the
/// publish flow's scan and passkey vouch only for a publish/<date>
/// snapshot. So every branch this declaration sends to the fork is under
/// publish/, and main — or any other branch — is refused here, at the
/// gate, before a converge could push it. The off-site disaster-recovery
/// copy of main belongs in a PRIVATE repository outside the fork network
/// (the item's second car), never in this list.
#[test]
fn the_public_fork_is_declared_publish_branches_only() {
    let d = declaration();
    let branches = d["branches"]
        .as_array()
        .expect("offsite-push.json declares a branches list");
    assert!(!branches.is_empty(), "offsite-push.json declares no branch");
    for b in branches {
        let name = b.as_str().unwrap_or_default();
        assert!(
            name.starts_with("publish/"),
            "offsite-push.json sends {b} to {} — a PUBLIC fork. Only publish/* \
             (scanned and passkey-signed by the publish flow) may go there; forge \
             main is not published unsigned (backlog 67931115)",
            d["remote"]
        );
    }
}

/// The declaration is the decided one: the canonical dauld/boss-mirror
/// URL (GitHub 301-redirects boss-fork there, and a push does not follow
/// redirects), and the same fork the publish verb opens its PRs from —
/// two spellings of one repository, pinned (CLAUDE.md §9a). Which
/// branches it may carry is the pin above.
#[test]
fn the_declaration_is_the_decided_one_and_matches_the_publish_fork() {
    let d = declaration();
    assert_eq!(
        d["remote"],
        serde_json::json!("https://github.com/dauld/boss-mirror.git")
    );
    let publish = std::fs::read_to_string(repo_root().join("infra/forge/publish-github-pr.sh"))
        .expect("publish-github-pr.sh");
    assert!(
        publish.contains(r#"FORK_SLUG="${BOSS_FORK_SLUG:-dauld/boss-mirror}""#)
            && publish.contains(r#"https://github.com/${FORK_SLUG}.git"#),
        "the publish verb's fork and the off-site push's remote must be one repository"
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
