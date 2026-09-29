//! `infra/forge/github-act.sh` — the three bounded GitHub verbs of design
//! 76155676 decision 4 (David 2026-09-27; backlog 6a8ff89f): create a
//! repository, set branch protection, delete named refs. Each is a
//! READ-ONLY plan verb and a passkey-approved write in design 17835005's
//! shape, authenticated as the GitHub App's installation on the owner the
//! request names, and each proves its act by reading GitHub back.
//!
//! Pinned here against a stub GitHub REST API (bash + jq, keeping
//! repositories and rulesets as files, and dressing a stored ruleset the
//! way GitHub does — ids, timestamps, reordered rules, parameter
//! defaults — so the read-back comparison is exercised on a real shape)
//! and against REAL git repositories standing in for GitHub's git side:
//!   * a plan is deterministic, names its act, and changes nothing; the
//!     write acts only on the plan's own hash and reads the act back;
//!     a second run of an applied plan is a refusal, not a second act
//!   * create-repository refuses a fork, the other visibility, a
//!     non-organisation owner; one already standing as declared is a plan
//!     with no act; a 201 that does not read back is a failure
//!   * set-branch-protection owns one ruleset per pattern with a fixed
//!     rule set, creates or replaces it, and proves it normalised
//!   * delete-refs deletes with one atomic, leased push; refuses the
//!     default branch, and main without a reason; a ref that moved voids
//!     the plan and nothing is deleted
//!   * the token slot: absent, loose, foreign-owned, expired or
//!     expiry-less is refused before GitHub is called, and the token
//!     never appears in any output or any argv
//!   * the six verb files hold the approval contract, and through the
//!     ops runner the plan verb is answered while the write, unapproved,
//!     never reaches GitHub

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const TOKEN: &str = "ghs_installation-token-must-never-print-0123456789";
const API: &str = "https://api.github.test";
const ORG: &str = "algedonic-dev";
const LIVE: &str = "2099-01-01T00:00:00Z";
const APPROVER: &str = "emp-david";
/// The call that ends the act's token (GitHub's installation-token revoke).
const REVOKE: &str = "DELETE /installation/token";
/// The broker rule github-act.sh renders algedonic-dev's slot from — the
/// first in the rules directory declaring `github-app-algedonic-dev`.
const ORG_RULE: &str = "infra/dispatcher/rules/broker-mints-the-algedonic-dev-admin-token-when-a-github-request-is-filed.toml";
/// The ops-request a run answers, as the runner hands every verb its
/// packet (`OPS_REQUEST_ID`, infra/ops/ops-runner.sh). The admin token is
/// keyed on it (backlog 4ce4ec55).
const REQUEST: &str = "4ce4ec55-6ee9-47ff-befe-6a97596a66d4";

fn script() -> PathBuf {
    repo_root().join("infra/forge/github-act.sh")
}

fn has(bin: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The script reads GitHub's answers with jq and hashes with sha256sum;
/// the stub is bash + jq. A box without them skips, saying so.
macro_rules! needs_tools {
    () => {
        if !(has("jq") && has("git") && has("sha256sum")) {
            eprintln!("skipping: needs jq, git and sha256sum");
            return;
        }
    };
}

/// The stub GitHub API. It is called in ONE shape (the script's `api`):
///   curl -q -sS -m 30 --max-redirs 0 -o <out> -w %{http_code}
///        -H @<auth> -H <accept> -H <version> -X <M>
///        [-H <content-type> --data-binary @<body>] <url>
/// It appends every argv to `$STUB_DIR/argv.log` (the token must never be
/// in one) and `<M> <path>` to `$STUB_LOG`, answers 401 unless the header
/// FILE carries the token, and keeps state under `$STUB_DIR/repos`.
/// Faults: `$STUB_DIR/fail` (curl exits 6), `$STUB_DIR/status_<M>`
/// (answer that status to every <M>), `$STUB_DIR/ignore_writes` (answer
/// a write as done and store nothing), `$STUB_DIR/empty_200` (answer
/// every GET 200 with NO body — the silence jq-1.6's `-e` reads as true),
/// `$STUB_DIR/ruleset_body` (answer GET .../rulesets/<id> ONLY with 200
/// and that file's bytes), `$STUB_DIR/create_as` (a JSON object merged
/// over the repository a POST creates), `$STUB_DIR/ruleset_integration`
/// (store every required check pinned to integration 999).
/// The installation token's end: `DELETE /installation/token` revokes it
/// (`$STUB_DIR/revoked`, after which every call answers 401, as GitHub's
/// do) unless `$STUB_DIR/revoke_ignored` — a 204 that does not take — and
/// `GET /installation/repositories` is the read that proves it dead.
const STUB: &str = r#"#!/usr/bin/env bash
set -u
{ printf '%s ' "$@"; echo; } >> "$STUB_DIR/argv.log"
out=""; method=GET; url=""; auth=""; data=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -X) method="$2"; shift 2 ;;
        -H) case "$2" in @*) auth="${2#@}" ;; esac; shift 2 ;;
        --data-binary) data="${2#@}"; shift 2 ;;
        -m|-w|--max-redirs) shift 2 ;;
        -*) shift ;;
        *) url="$1"; shift ;;
    esac
done
path="${url#"$STUB_API"}"
bare="${path%%\?*}"
echo "$method $path" >> "$STUB_LOG"
[ -e "$STUB_DIR/fail" ] && { echo "curl: (6) Could not resolve host: api.github.test" >&2; exit 6; }
reply() { printf '%s' "$2" > "$out"; printf '%s' "$1"; exit 0; }
# The read that proves a revoke, answering a set status whatever the token:
# a 403 (a rate limit) or a 404 is not the 401 that proves it dead.
[ "$method $bare" = "GET /installation/repositories" ] && [ -e "$STUB_DIR/verify_status" ] \
    && reply "$(cat "$STUB_DIR/verify_status")" '{"message":"stub verify answer"}'
# Whose token: $STUB_TOKEN (revoked is `$STUB_DIR/revoked`), or one listed
# in `$STUB_DIR/more_tokens` (revoked are listed in `revoked_more`) — two
# requests' tokens live side by side, and ending one leaves the other.
tok="$(sed -n 's/^Authorization: Bearer //p' "$auth" 2>/dev/null)"
if [ "$tok" = "$STUB_TOKEN" ]; then
    [ -e "$STUB_DIR/revoked" ] && reply 401 '{"message":"Bad credentials"}'
elif [ -n "$tok" ] && grep -qxF -- "$tok" "$STUB_DIR/more_tokens" 2>/dev/null; then
    grep -qxF -- "$tok" "$STUB_DIR/revoked_more" 2>/dev/null && reply 401 '{"message":"Bad credentials"}'
else
    reply 401 '{"message":"Bad credentials"}'
fi
case "$method $bare" in
    "DELETE /installation/token")
        if [ ! -e "$STUB_DIR/revoke_ignored" ]; then
            if [ "$tok" = "$STUB_TOKEN" ]; then : > "$STUB_DIR/revoked"
            else printf '%s\n' "$tok" >> "$STUB_DIR/revoked_more"; fi
        fi
        reply 204 '' ;;
    "GET /installation/repositories")
        reply 200 '{"total_count":1,"repositories":[]}' ;;
esac
[ -e "$STUB_DIR/empty_200" ] && [ "$method" = GET ] && reply 200 ''
[ -e "$STUB_DIR/status_$method" ] && reply "$(cat "$STUB_DIR/status_$method")" '{"message":"Validation Failed","errors":[{"message":"stub refusal"}]}'
R="$STUB_DIR/repos"
# server <id> <owner/repo> <body file> — a ruleset as GitHub returns it.
server() {
    pin=false; [ -e "$STUB_DIR/ruleset_integration" ] && pin=true
    jq -c --argjson id "$1" --arg src "$2" --argjson pin "$pin" '. + {id: $id, source_type: "Repository", source: $src,
        created_at: "2026-09-28T00:00:00Z", updated_at: "2026-09-28T00:00:00Z", node_id: "RRS_stub",
        _links: {self: {href: "https://api.github.test/x"}}, current_user_can_bypass: "always"}
        | .rules |= (reverse | map(if .type == "required_status_checks"
                                  then .parameters.do_not_enforce_on_create = false else . end))
        | .bypass_actors |= reverse
        | if $pin then .rules |= map(if .type == "required_status_checks"
              then .parameters.required_status_checks |= map(.integration_id = 999) else . end)
          else . end' "$3"
}
case "$method $bare" in
    "GET /orgs/"*)
        f="$STUB_DIR/org_${bare#/orgs/}.json"
        [ -e "$f" ] && reply 200 "$(cat "$f")"
        reply 404 '{"message":"Not Found"}' ;;
    "POST /orgs/"*/repos)
        o="${bare#/orgs/}"; o="${o%/repos}"
        n="$(jq -r .name "$data")"; f="$R/$o/$n.json"
        [ -e "$f" ] && reply 422 '{"message":"Repository creation failed.","errors":[{"message":"name already exists on this account"}]}'
        body="$(jq -c --arg o "$o" '{id: 777, name, full_name: "\($o)/\(.name)", owner: {login: $o},
            private, visibility, fork: false, archived: false, default_branch: "main"}' "$data")"
        [ -e "$STUB_DIR/create_as" ] && body="$(jq -c --argjson o "$(cat "$STUB_DIR/create_as")" '. + $o' <<<"$body")"
        [ -e "$STUB_DIR/ignore_writes" ] || { mkdir -p "$R/$o"; printf '%s' "$body" > "$f"; }
        reply 201 "$body" ;;
    "GET /repos/"*/*/rulesets/*)
        [ -e "$STUB_DIR/ruleset_body" ] && reply 200 "$(cat "$STUB_DIR/ruleset_body")"
        rest="${bare#/repos/}"; f="$R/${rest%/rulesets/*}.rulesets/${rest##*/}.json"
        [ -e "$f" ] && reply 200 "$(cat "$f")"
        reply 404 '{"message":"Not Found"}' ;;
    "PUT /repos/"*/*/rulesets/*)
        rest="${bare#/repos/}"; or="${rest%/rulesets/*}"; id="${rest##*/}"; f="$R/$or.rulesets/$id.json"
        [ -e "$f" ] || reply 404 '{"message":"Not Found"}'
        body="$(server "$id" "$or" "$data")"
        [ -e "$STUB_DIR/ignore_writes" ] || printf '%s' "$body" > "$f"
        reply 200 "$body" ;;
    "GET /repos/"*/*/rulesets)
        rest="${bare#/repos/}"; d="$R/${rest%/rulesets}.rulesets"
        if ls "$d"/*.json >/dev/null 2>&1; then
            reply 200 "$(jq -s -c 'map({id, name, target, source_type, source, enforcement})' "$d"/*.json)"
        fi
        reply 200 '[]' ;;
    "POST /repos/"*/*/rulesets)
        rest="${bare#/repos/}"; or="${rest%/rulesets}"; d="$R/$or.rulesets"
        mkdir -p "$d"
        id=$(( $(ls "$d" | wc -l) + 101 ))
        body="$(server "$id" "$or" "$data")"
        [ -e "$STUB_DIR/ignore_writes" ] || printf '%s' "$body" > "$d/$id.json"
        reply 201 "$body" ;;
    "GET /repos/"*)
        f="$R/${bare#/repos/}.json"
        [ -e "$f" ] && reply 200 "$(cat "$f")"
        reply 404 '{"message":"Not Found"}' ;;
esac
reply 599 '{"message":"the stub has no route for this call"}'
"#;

/// The render github-act.sh runs before it reads its slot and, with
/// `--expire`, after its act (infra/forge/credential-render.sh, pinned on
/// its own in credential_render_sh.rs against a stub kubectl). This stub
/// records each call's arguments in `$STUB_DIR/render.log` — prefixed
/// `after-revoke` when GitHub had already been asked to revoke, and with
/// `used=token` when the file `--expire` names holds the token the act
/// used — and exits `$STUB_DIR/render_rc` (0). The slot itself is placed
/// by the test, as the broker and a real render would leave it — or, with
/// `$STUB_DIR/mint_late`, by this stub's SECOND call: the broker's mint
/// landing after the runner's first pass.
const RENDER_STUB: &str = r#"#!/usr/bin/env bash
set -u
if [ -e "$STUB_DIR/mint_late" ] && [ "$(wc -l < "$STUB_DIR/render.log" 2>/dev/null || echo 0)" = 1 ]; then
    (umask 077 && printf '%s' "$STUB_TOKEN" > "$4" && printf '2099-01-01T00:00:00Z' > "$4.expires-at" \
        && printf '%s' "$6" > "$4.request")
fi
line="$*"
[ -e "$STUB_DIR/revoked" ] && line="after-revoke $line"
used="" prev=""
for a in "$@"; do [ "$prev" = --expire ] && used="$a"; prev="$a"; done
if [ -n "$used" ] && [ "$(cat "$used" 2>/dev/null)" = "$STUB_TOKEN" ]; then line="$line used=token"; fi
printf '%s\n' "$line" >> "$STUB_DIR/render.log"
exit "$(cat "$STUB_DIR/render_rc" 2>/dev/null || echo 0)"
"#;

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn text(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
    fn plan_sha(&self) -> String {
        assert_eq!(self.code, 0, "the plan did not render:\n{}", self.text());
        self.stderr
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("no plan-sha256 on stderr:\n{}", self.text()))
            .to_string()
    }
}

struct Gh {
    root: PathBuf,
    stub: PathBuf,
    state: PathBuf,
    tokens: PathBuf,
    github: PathBuf,
    /// The unit's RuntimeDirectory= as systemd hands it to the runner and
    /// every verb (RUNTIME_DIRECTORY): 0700, root's on the forge.
    run_dir: PathBuf,
    render: PathBuf,
    uid: u32,
}

impl Gh {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("github-act-{name}"));
        let state = root.join("stub");
        let tokens = root.join("github-app");
        let github = root.join("github");
        let run_dir = root.join("run");
        for d in [&state, &tokens, &github, &run_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        // mode-bits-ok: a directory, the 0700 RuntimeDirectory= systemd makes; nothing execs it
        std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir_all(state.join("repos")).unwrap();
        let stub = root.join("github-curl");
        write_exec(&stub, STUB);
        let render = root.join("render");
        write_exec(&render, RENDER_STUB);
        write_file(
            &state.join(format!("org_{ORG}.json")),
            &json!({"login": ORG, "id": 4242, "type": "Organization"}).to_string(),
        );
        write_file(&state.join("log"), "");
        write_file(&state.join("argv.log"), "");
        let uid = std::fs::metadata(&root).unwrap().uid();
        let gh = Gh {
            root,
            stub,
            state,
            tokens,
            github,
            run_dir,
            render,
            uid,
        };
        gh.slot(ORG, TOKEN, Some(LIVE));
        gh
    }

    /// The installation token slot for `owner`, as credential-render.sh
    /// leaves it: the token, 0600, with its expiry beside it and the
    /// request it was rendered for (this run's, [`REQUEST`]).
    fn slot(&self, owner: &str, token: &str, expires: Option<&str>) {
        let f = self.tokens.join(format!("{owner}.token"));
        write_file(&f, token);
        write_file(&self.tokens.join(format!("{owner}.token.request")), REQUEST);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        let e = self.tokens.join(format!("{owner}.token.expires-at"));
        match expires {
            Some(at) => write_file(&e, at),
            None => {
                let _ = std::fs::remove_file(&e);
            }
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new("bash");
        c.arg(script()).args(args);
        // The ops runner's environment: no HOME, nothing from the packet.
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            // systemd sets this for boss-ops-runner.service's
            // RuntimeDirectory= (backlog 5f77b205); it is not a seam.
            .env("RUNTIME_DIRECTORY", &self.run_dir)
            // The runner hands every verb the request it answers; the
            // admin token is keyed on it (backlog 4ce4ec55). Not a seam.
            .env("OPS_REQUEST_ID", REQUEST)
            // Without this marker the script drops every BOSS_GITHUB_*
            // seam and reads the forge's real slot and GitHub.
            .env("BOSS_GITHUB_ACT_SEAMS", "test")
            .env("BOSS_GITHUB_APP_TOKEN_DIR", &self.tokens)
            .env("BOSS_GITHUB_TOKEN_OWNER_UID", self.uid.to_string())
            .env("BOSS_GITHUB_API", API)
            .env("BOSS_GITHUB_CURL", &self.stub)
            .env("BOSS_GITHUB_GIT_BASE", &self.github)
            .env("BOSS_GITHUB_READBACK_SLEEP", "0")
            .env("BOSS_GITHUB_RENDER", &self.render)
            .env("STUB_DIR", &self.state)
            .env("STUB_LOG", self.state.join("log"))
            .env("STUB_TOKEN", TOKEN)
            .env("STUB_API", API);
        c
    }

    fn run(&self, args: &[&str]) -> Out {
        self.run_env(args, &[])
    }

    /// `run` with extra environment — what a runner unit might leak in.
    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let mut c = self.cmd(args);
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().expect("github-act.sh runs");
        let o = Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        assert!(
            !o.text().contains(TOKEN) && !self.argv_log().contains(TOKEN),
            "the token reached an output or a curl argv:\n{}\nargv:\n{}",
            o.text(),
            self.argv_log()
        );
        o
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.state.join("log")).unwrap_or_default()
    }

    fn argv_log(&self) -> String {
        std::fs::read_to_string(self.state.join("argv.log")).unwrap_or_default()
    }

    /// What the act wrote to GitHub — not the revoke of its own token.
    fn writes(&self) -> Vec<String> {
        self.log()
            .lines()
            .filter(|l| !l.starts_with("GET ") && *l != REVOKE)
            .map(str::to_string)
            .collect()
    }

    fn revokes(&self) -> usize {
        self.log().lines().filter(|l| *l == REVOKE).count()
    }

    fn renders(&self) -> Vec<String> {
        std::fs::read_to_string(self.state.join("render.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn slot_held(&self, owner: &str) -> bool {
        self.tokens.join(format!("{owner}.token")).exists()
    }

    /// The broker's next mint, for the next act: an act revokes its token
    /// and removes the slot, so the one after it needs a fresh one (the
    /// filing or approval rule's work, design 76c46869).
    fn remint(&self) {
        let _ = std::fs::remove_file(self.state.join("revoked"));
        self.slot(ORG, TOKEN, Some(LIVE));
    }

    fn fault(&self, name: &str, body: &str) {
        write_file(&self.state.join(name), body);
    }

    fn put_repo(&self, owner: &str, name: &str, v: Value) {
        let d = self.state.join("repos").join(owner);
        std::fs::create_dir_all(&d).unwrap();
        write_file(&d.join(format!("{name}.json")), &v.to_string());
    }

    fn repo(&self, owner: &str, name: &str) -> Option<Value> {
        std::fs::read_to_string(self.state.join(format!("repos/{owner}/{name}.json")))
            .ok()
            .map(|s| serde_json::from_str(&s).unwrap())
    }

    fn rulesets(&self, owner: &str, name: &str) -> Vec<Value> {
        let d = self.state.join(format!("repos/{owner}/{name}.rulesets"));
        let Ok(rd) = std::fs::read_dir(&d) else {
            return vec![];
        };
        let mut v: Vec<Value> = rd
            .map(|e| {
                serde_json::from_str(&std::fs::read_to_string(e.unwrap().path()).unwrap()).unwrap()
            })
            .collect();
        v.sort_by_key(|r| r["id"].as_i64());
        v
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what} does not say `{n}`:\n{text}");
    }
}

// ---------------------------------------------------------------------------
// create-repository
// ---------------------------------------------------------------------------

#[test]
fn a_repository_plan_names_its_act_changes_nothing_and_renders_the_same_twice() {
    needs_tools!();
    let g = Gh::new("create-plan");
    let a = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    let sha = a.plan_sha();
    contains_all(
        &a.stdout,
        &[
            "plan: github-create-repository",
            "repository: algedonic-dev/boss-dr",
            "organisation: algedonic-dev (id 4242)",
            "state: absent",
            r#"act: POST /orgs/algedonic-dev/repos {"name":"boss-dr","private":true,"visibility":"private","auto_init":false}"#,
            "never: a fork",
        ],
        "the plan",
    );
    let b = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(a.stdout, b.stdout, "two renders of one state differ");
    assert_eq!(sha, b.plan_sha());
    assert!(g.writes().is_empty(), "a plan wrote: {:?}", g.writes());
    assert!(g.repo(ORG, "boss-dr").is_none());
}

#[test]
fn the_write_creates_on_the_signed_plan_proves_it_and_refuses_a_second_run() {
    needs_tools!();
    let g = Gh::new("create-write");
    let sha = g
        .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .plan_sha();
    let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(w.code, 0, "the write failed:\n{}", w.text());
    contains_all(
        &w.text(),
        &[
            "plan: github-create-repository",
            "still holds",
            "answered 201",
            "proven — GET /repos/algedonic-dev/boss-dr reads back as algedonic-dev/boss-dr, private true, fork false",
        ],
        "the write",
    );
    assert_eq!(
        g.writes(),
        vec!["POST /orgs/algedonic-dev/repos".to_string()]
    );
    let r = g.repo(ORG, "boss-dr").expect("the repository exists");
    assert_eq!(r["private"], true);
    assert_eq!(r["fork"], false);
    assert_eq!(g.revokes(), 1, "the act's token outlived it:\n{}", g.log());

    // The state moved, so the signed plan no longer holds: no second act,
    // even with a token minted for it.
    g.remint();
    let again = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(
        again.code,
        78,
        "a second run was not refused:\n{}",
        again.text()
    );
    contains_all(
        &again.text(),
        &["not the approved", "state: exists as declared"],
        "the refusal",
    );
    assert_eq!(g.writes().len(), 1, "a second run wrote: {:?}", g.writes());
}

#[test]
fn a_repository_that_already_stands_as_declared_is_a_plan_with_no_act() {
    needs_tools!();
    let g = Gh::new("create-exists");
    g.put_repo(
        ORG,
        "boss-dr",
        json!({"id": 9, "full_name": "algedonic-dev/boss-dr", "private": true, "visibility": "private", "fork": false, "archived": false}),
    );
    let p = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    contains_all(
        &p.stdout,
        &[
            "state: exists as declared — id 9, visibility private, private true, fork false, not archived",
            "act: none",
        ],
        "the plan",
    );
    let w = g.run(&[
        "create-repository",
        ORG,
        "boss-dr",
        "private",
        &p.plan_sha(),
    ]);
    assert_eq!(w.code, 0, "{}", w.text());
    assert!(w.stdout.contains("nothing was created"), "{}", w.text());
    assert!(g.writes().is_empty(), "{:?}", g.writes());
}

#[test]
fn a_fork_the_other_visibility_or_a_user_owner_is_refused() {
    needs_tools!();
    let g = Gh::new("create-refuse");
    g.put_repo(
        ORG,
        "forked",
        json!({"full_name": "algedonic-dev/forked", "private": false, "fork": true, "parent": {"full_name": "someone/else"}}),
    );
    g.put_repo(
        ORG,
        "public-one",
        json!({"full_name": "algedonic-dev/public-one", "private": false, "fork": false}),
    );
    for (name, vis, words) in [
        ("forked", "public", "is a FORK (of someone/else)"),
        ("public-one", "private", "never changes one's visibility"),
    ] {
        let o = g.run(&["create-repository", "--plan", ORG, name, vis]);
        assert_eq!(o.code, 78, "{name} was not refused:\n{}", o.text());
        assert!(o.stderr.contains(words), "{name}: {}", o.stderr);
    }
    // An owner GitHub does not answer as an organisation (a user account).
    let user = Gh::new("create-user-owner");
    let _ = std::fs::remove_file(user.state.join(format!("org_{ORG}.json")));
    let o = user.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(o.code, 78, "{}", o.text());
    assert!(o.stderr.contains("is not an organisation"), "{}", o.stderr);
    for bad in [
        vec!["create-repository", "--plan", ORG, "-rf", "private"],
        vec!["create-repository", "--plan", ORG, "boss.git", "private"],
        vec!["create-repository", "--plan", ORG, "boss-dr", "internal"],
        vec![
            "create-repository",
            "--plan",
            "../etc",
            "boss-dr",
            "private",
        ],
    ] {
        let o = g.run(&bad);
        assert_eq!(o.code, 78, "{bad:?} was not refused:\n{}", o.text());
    }
    assert!(g.writes().is_empty(), "{:?}", g.writes());
}

#[test]
fn a_create_that_does_not_read_back_or_is_refused_fails_naming_github() {
    needs_tools!();
    let g = Gh::new("create-fail");
    let sha = g
        .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .plan_sha();
    g.fault("ignore_writes", "");
    let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(w.code, 1, "{}", w.text());
    contains_all(
        &w.stderr,
        &["does not read back", "answered 201"],
        "the failure",
    );

    let g = Gh::new("create-422");
    let sha = g
        .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .plan_sha();
    g.fault("status_POST", "422");
    let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(w.code, 1, "{}", w.text());
    contains_all(
        &w.stderr,
        &["answered HTTP 422", "Validation Failed; stub refusal"],
        "the failure",
    );

    let g = Gh::new("create-dark");
    g.fault("fail", "");
    let p = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(
        p.code,
        1,
        "an unreachable GitHub rendered a plan:\n{}",
        p.text()
    );
    assert!(p.stderr.contains("did not answer"), "{}", p.stderr);
}

/// A 200 with no body is no answer. On jq-1.6 `jq -e` over no document
/// exits 0 (backlog d96e38ab), so without `jq_doc_file` in front of each
/// guard an empty body would pass as a repository that is not archived
/// and a ruleset list with room to spare.
#[test]
fn a_200_with_no_body_is_no_answer() {
    needs_tools!();
    let g = Gh::new("empty-body");
    put_boss_dr(&g);
    g.fault("empty_200", "");
    let o = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(
        o.code,
        1,
        "an empty organisation answer rendered a plan:\n{}",
        o.text()
    );
    assert!(o.stderr.contains("no JSON document"), "{}", o.stderr);
    let o = g.run(&[
        "set-branch-protection",
        "--plan",
        ORG,
        "boss-dr",
        "main",
        "none",
        "none",
    ]);
    assert_eq!(
        o.code,
        1,
        "an empty repository answer rendered a plan:\n{}",
        o.text()
    );
    assert!(o.stderr.contains("no JSON document"), "{}", o.stderr);
    assert!(g.writes().is_empty(), "{:?}", g.writes());
}

// ---------------------------------------------------------------------------
// the token slot
// ---------------------------------------------------------------------------

#[test]
fn a_missing_loose_foreign_expired_or_expiry_less_slot_is_refused_before_github() {
    needs_tools!();
    let plan = ["create-repository", "--plan", ORG, "boss-dr", "private"];

    let g = Gh::new("slot-absent");
    let _ = std::fs::remove_file(g.tokens.join(format!("{ORG}.token")));
    let o = g.run(&plan);
    assert_eq!(o.code, 78);
    contains_all(
        &o.stderr,
        &[
            &format!("{}/{ORG}.token", g.tokens.display()),
            "credential-render.sh",
            "installation_id",
        ],
        "the absent-slot refusal",
    );

    let g = Gh::new("slot-loose");
    std::fs::set_permissions(
        g.tokens.join(format!("{ORG}.token")),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let o = g.run(&plan);
    assert_eq!(o.code, 78);
    assert!(o.stderr.contains("mode 644"), "{}", o.stderr);

    let g = Gh::new("slot-expired");
    g.slot(ORG, TOKEN, Some("2020-01-01T00:00:00Z"));
    let o = g.run(&plan);
    assert_eq!(o.code, 78);
    assert!(
        o.stderr.contains("expired at 2020-01-01T00:00:00Z"),
        "{}",
        o.stderr
    );

    let g = Gh::new("slot-no-expiry");
    g.slot(ORG, TOKEN, None);
    let o = g.run(&plan);
    assert_eq!(o.code, 78);
    assert!(o.stderr.contains("no recorded expiry"), "{}", o.stderr);

    let g = Gh::new("slot-foreign");
    let mut c = g.cmd(&plan);
    let out = c
        .env("BOSS_GITHUB_TOKEN_OWNER_UID", (g.uid + 1).to_string())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&out.stderr).contains("must be owned by uid"));

    let g = Gh::new("slot-link");
    let f = g.tokens.join(format!("{ORG}.token"));
    let real = g.root.join("elsewhere.token");
    std::fs::rename(&f, &real).unwrap();
    std::os::unix::fs::symlink(&real, &f).unwrap();
    let o = g.run(&plan);
    assert_eq!(o.code, 78);
    assert!(o.stderr.contains("not a regular file"), "{}", o.stderr);

    let g = Gh::new("slot-quiet");
    let _ = std::fs::remove_file(g.tokens.join(format!("{ORG}.token")));
    g.run(&plan);
    assert!(
        g.log().is_empty(),
        "GitHub was called without a slot: {}",
        g.log()
    );
}

// ---------------------------------------------------------------------------
// the act's own token: rendered before, revoked after (design 76c46869 Q1-Q2,
// David 2026-09-28; backlog bf8726c9). The algedonic-dev admin token is
// minted per act and never kept alive, so the verb renders its slot from the
// broker's Secret itself — no wait of up to ten minutes for a converge — and
// once a write is over it marks the Secret's copy expired, revokes the token
// at GitHub, proves it dead, and removes the slot.
// ---------------------------------------------------------------------------

#[test]
fn every_act_renders_its_owners_slot_from_the_broker_rule_first() {
    needs_tools!();
    let g = Gh::new("render-first");
    let p = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(p.code, 0, "{}", p.text());
    let rule = repo_root().join(ORG_RULE);
    let slot = g.tokens.join(format!("{ORG}.token"));
    assert_eq!(
        g.renders(),
        vec![format!(
            "--rule {} --dest {} --request {REQUEST}",
            rule.display(),
            slot.display()
        )],
        "the plan renders algedonic-dev's slot from the rule declaring github-app-algedonic-dev, \
         from the key of the request it answers (backlog 4ce4ec55)"
    );
    assert!(
        !p.stdout.contains("--rule"),
        "the render's report rides stderr, never the plan's bytes:\n{}",
        p.stdout
    );
    // A plan is not the act: the token stays for the approved write.
    assert_eq!(g.revokes(), 0, "{}", g.log());
    assert!(g.slot_held(ORG));

    // A render that refuses its invocation is a refusal before GitHub.
    let g = Gh::new("render-refused");
    g.fault("render_rc", "78");
    let o = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(o.code, 78, "{}", o.text());
    assert!(o.stderr.contains("exit 78"), "{}", o.stderr);
    assert!(g.log().is_empty(), "{}", g.log());

    // A render that names a fault (exit 1) leaves the slot to say what
    // stands: here a held live token, so the plan renders.
    let g = Gh::new("render-fault");
    g.fault("render_rc", "1");
    let o = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(o.code, 0, "{}", o.text());
}

/// The filing rule mints within seconds of the request, and the runner's
/// pass can come sooner: a plan that refused on that race would abort a
/// request David filed. An absent slot is rendered again, three times.
#[test]
fn a_mint_still_in_flight_is_rendered_again_before_any_refusal() {
    needs_tools!();
    let g = Gh::new("mint-late");
    let _ = std::fs::remove_file(g.tokens.join(format!("{ORG}.token")));
    let _ = std::fs::remove_file(g.tokens.join(format!("{ORG}.token.expires-at")));
    g.fault("mint_late", "");
    let p = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(p.code, 0, "{}", p.text());
    assert_eq!(g.renders().len(), 2, "{:?}", g.renders());
    assert!(p.stderr.contains("try 1 of 3"), "{}", p.stderr);

    // Never minted: three renders, then the refusal, before GitHub.
    let g = Gh::new("mint-never");
    let _ = std::fs::remove_file(g.tokens.join(format!("{ORG}.token")));
    let o = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(o.code, 78, "{}", o.text());
    assert_eq!(g.renders().len(), 3, "{:?}", g.renders());
    assert!(g.log().is_empty(), "{}", g.log());
}

#[test]
fn an_owner_no_broker_rule_declares_is_refused_before_github() {
    needs_tools!();
    let g = Gh::new("no-rule");
    g.slot("dauld", TOKEN, Some(LIVE));
    let o = g.run(&["create-repository", "--plan", "dauld", "boss-dr", "private"]);
    assert_eq!(o.code, 78, "{}", o.text());
    contains_all(
        &o.stderr,
        &["github-app-dauld", "no broker rule"],
        "the refusal",
    );
    assert!(g.log().is_empty(), "{}", g.log());
    assert!(g.renders().is_empty(), "{:?}", g.renders());
}

#[test]
fn a_write_marks_its_token_expired_then_revokes_it_and_removes_the_slot() {
    needs_tools!();
    let g = Gh::new("revoke");
    let sha = g
        .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .plan_sha();
    let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(w.code, 0, "{}", w.text());
    let log = g.log();
    let at = |line: &str| {
        log.lines()
            .position(|l| l.starts_with(line))
            .unwrap_or_else(|| panic!("no `{line}` in:\n{log}"))
    };
    assert!(
        at("POST /orgs/algedonic-dev/repos") < at(REVOKE)
            && at("GET /repos/algedonic-dev/boss-dr") < at(REVOKE),
        "the revoke comes after the act and its read-back:\n{log}"
    );
    assert!(
        at(REVOKE) < at("GET /installation/repositories"),
        "the token is proven dead after the revoke:\n{log}"
    );
    let rule = repo_root().join(ORG_RULE);
    let slot = g.tokens.join(format!("{ORG}.token"));
    let renders = g.renders();
    assert_eq!(renders.len(), 3, "plan, write, expire: {renders:?}");
    let expire = &renders[2];
    assert!(
        expire.starts_with(&format!(
            "--rule {} --dest {} --request {REQUEST} --expire ",
            rule.display(),
            slot.display()
        )) && expire.ends_with("used=token"),
        "the Secret's copy — the request's own key — is marked expired, naming the token the \
         act used: {expire}"
    );
    assert!(
        !expire.starts_with("after-revoke"),
        "the expiry is marked BEFORE the revoke, as the broker's own is — a death between \
         the two must not leave a dead token that reads as live: {renders:?}"
    );
    assert!(
        !g.slot_held(ORG) && !g.tokens.join(format!("{ORG}.token.expires-at")).exists(),
        "the slot outlived the act"
    );
    contains_all(
        &w.stderr,
        &[
            "DELETE /installation/token answered 204",
            "GET /installation/repositories",
            "answers 401",
        ],
        "the revoke",
    );
}

#[test]
fn a_refused_or_failed_write_revokes_its_token_too() {
    needs_tools!();
    // Refused: the state moved since the plan was signed.
    let g = Gh::new("revoke-refused");
    let sha = g
        .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .plan_sha();
    put_boss_dr(&g);
    let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(w.code, 78, "{}", w.text());
    assert!(w.stderr.contains("not the approved"), "{}", w.stderr);
    assert_eq!(
        g.revokes(),
        1,
        "a refused act's token lived on:\n{}",
        g.log()
    );
    assert!(!g.slot_held(ORG));

    // Failed: GitHub refused the write.
    let g = Gh::new("revoke-failed");
    let sha = g
        .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .plan_sha();
    g.fault("status_POST", "422");
    let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(w.code, 1, "{}", w.text());
    assert_eq!(g.revokes(), 1, "{}", g.log());
    assert!(!g.slot_held(ORG));
}

#[test]
fn a_revoke_that_does_not_take_fails_the_act_it_follows() {
    needs_tools!();
    let g = Gh::new("revoke-ignored");
    let sha = g
        .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .plan_sha();
    g.fault("revoke_ignored", "");
    let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(
        w.code,
        1,
        "a token that still authenticates after its act passed:\n{}",
        w.text()
    );
    contains_all(
        &w.text(),
        &[
            "proven — GET /repos/algedonic-dev/boss-dr",
            "STILL AUTHENTICATES",
            "the act itself is done and read back",
        ],
        "the failure",
    );
    assert!(!g.slot_held(ORG), "the slot outlived the act");

    // The Secret's expiry not marked: the revoke still runs, and it is red.
    let g = Gh::new("expire-unmarked");
    let sha = g
        .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .plan_sha();
    g.fault("render_rc", "1");
    let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
    assert_eq!(w.code, 1, "{}", w.text());
    assert_eq!(g.revokes(), 1, "{}", g.log());
    assert!(w.stderr.contains("NOT marked"), "{}", w.stderr);
}

/// Only a 401 proves a revoke (review of car 57a2c56e, finding 3: widening
/// the check to any 4xx survived every test here). A 403 is a rate limit
/// or a permission answer, and a 404 an endpoint that did not find the
/// installation: GitHub accepted the credential either way, so the write
/// that follows either is exit 1, naming what answered.
#[test]
fn a_verify_read_answering_403_or_404_does_not_prove_the_revoke() {
    needs_tools!();
    for code in ["403", "404"] {
        let g = Gh::new(&format!("verify-{code}"));
        let sha = g
            .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
            .plan_sha();
        g.fault("verify_status", code);
        let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
        assert_eq!(
            w.code,
            1,
            "a verify read answering {code} passed as proof of the revoke:\n{}",
            w.text()
        );
        contains_all(
            &w.text(),
            &[
                "DELETE /installation/token answered 204",
                &format!("answered HTTP {code}"),
                "not proven revoked",
                "the act itself is done and read back",
            ],
            "the failure",
        );
    }
}

/// The admin token is keyed on the request (backlog 4ce4ec55), and the
/// runner hands every verb the packet it answers as `OPS_REQUEST_ID`. A
/// run without one — or with anything but a packet id, which becomes part
/// of a Secret key — cannot know whose token is its own, and is refused
/// before a render or a call to GitHub.
#[test]
fn a_run_without_its_request_is_refused_before_github() {
    needs_tools!();
    for request in [
        None,
        Some(""),
        Some("../../token"),
        Some("4CE4EC55-6EE9-47FF-BEFE-6A97596A66D4"),
        Some("4ce4ec55"),
    ] {
        let g = Gh::new("no-request");
        let mut c = g.cmd(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
        match request {
            Some(r) => c.env("OPS_REQUEST_ID", r),
            None => c.env_remove("OPS_REQUEST_ID"),
        };
        let out = c.output().expect("github-act.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.code(), Some(78), "{request:?}: {text}");
        assert!(text.contains("OPS_REQUEST_ID"), "{request:?}: {text}");
        assert!(g.renders().is_empty(), "{request:?}: {:?}", g.renders());
        assert!(g.log().is_empty(), "{request:?}: {}", g.log());
    }
}

/// THE STRAND, REPRODUCED (backlog 4ce4ec55, the review of car 57a2c56e,
/// finding 1). Two GitHub writes on algedonic-dev are approved before the
/// runner executes the first. Until this car the broker kept ONE key per
/// owner: approval 1 minted into it, approval 2 found that token fresh and
/// minted nothing, write 1 marked the key expired and revoked its token,
/// and write 2 — its plan re-run before the claim first — rendered the
/// expired key three times and refused 78, its single-use approval spent.
///
/// Driven end to end: the REAL render (infra/forge/credential-render.sh)
/// against the stub kubectl, a GitHub that knows two live tokens, and each
/// run handed its own request as the runner hands it. The Secret holds
/// both shapes the broker has written — the one shared key (`token`, the
/// value both approvals found) and the per-request keys it writes now —
/// so the same fixture strands write 2 wherever the verb reads the shared
/// key, and passes only where each request reads its own.
#[test]
fn two_approved_writes_for_one_owner_each_act_with_their_own_token() {
    needs_tools!();
    const R1: &str = "11111111-1111-4111-8111-111111111111";
    const R2: &str = "22222222-2222-4222-8222-222222222222";
    const TOKEN2: &str = "ghs_second-requests-token-must-never-print-98765";
    let g = Gh::new("two-writes");
    // No slot placed by hand: the render places it from the Secret.
    for f in [
        format!("{ORG}.token"),
        format!("{ORG}.token.expires-at"),
        format!("{ORG}.token.request"),
    ] {
        let _ = std::fs::remove_file(g.tokens.join(f));
    }
    write_file(&g.state.join("more_tokens"), &format!("{TOKEN2}\n"));
    let secrets = g.root.join("secrets");
    let secret = secrets.join("boss").join("github-app-algedonic-dev");
    std::fs::create_dir_all(&secret).unwrap();
    for (k, v) in [
        ("token".to_string(), TOKEN),
        ("token.expires-at".to_string(), LIVE),
        (format!("token-{R1}"), TOKEN),
        (format!("token-{R1}.expires-at"), LIVE),
        (format!("token-{R1}.minted-for"), R1),
        (format!("token-{R2}"), TOKEN2),
        (format!("token-{R2}.expires-at"), LIVE),
        (format!("token-{R2}.minted-for"), R2),
    ] {
        write_file(&secret.join(k), v);
    }
    let kubectl = g.root.join("kubectl");
    write_exec(&kubectl, boss_testing::kubectl_secret_stub::SECRET_KUBECTL);
    let render = repo_root().join("infra/forge/credential-render.sh");
    let as_request = |request: &str, args: &[&str]| {
        let o = g.run_env(
            args,
            &[
                ("OPS_REQUEST_ID", request),
                ("BOSS_GITHUB_RENDER", &render.display().to_string()),
                ("BOSS_RENDER_KUBECTL", &kubectl.display().to_string()),
                ("STUB_SECRETS", &secrets.display().to_string()),
            ],
        );
        assert!(
            !o.text().contains(TOKEN2) && !g.argv_log().contains(TOKEN2),
            "the second token reached an output or a curl argv:\n{}",
            o.text()
        );
        o
    };

    // Both plans render and are signed before either write runs.
    let sha1 = as_request(
        R1,
        &["create-repository", "--plan", ORG, "boss-dr", "private"],
    )
    .plan_sha();
    let sha2 = as_request(
        R2,
        &["create-repository", "--plan", ORG, "boss-two", "private"],
    )
    .plan_sha();

    let w1 = as_request(R1, &["create-repository", ORG, "boss-dr", "private", &sha1]);
    assert_eq!(w1.code, 0, "write 1 failed:\n{}", w1.text());
    assert!(g.repo(ORG, "boss-dr").is_some());
    let revoked_more = std::fs::read_to_string(g.state.join("revoked_more")).unwrap_or_default();
    assert!(
        !revoked_more.contains(TOKEN2),
        "write 1 revoked the token request 2 acts with"
    );

    // The runner re-runs the plan verb before it claims execute (the
    // ops-runner.sh header): request 2's re-render must find its own key.
    let again = as_request(
        R2,
        &["create-repository", "--plan", ORG, "boss-two", "private"],
    );
    assert_eq!(
        again.code,
        0,
        "request 2's plan re-run was stranded by write 1's revoke:\n{}",
        again.text()
    );
    assert_eq!(again.plan_sha(), sha2);
    let w2 = as_request(
        R2,
        &["create-repository", ORG, "boss-two", "private", &sha2],
    );
    assert_eq!(w2.code, 0, "write 2 failed:\n{}", w2.text());
    assert!(g.repo(ORG, "boss-two").is_some());

    // Each write ended its own token, and only its own key reads expired.
    assert_eq!(g.revokes(), 2, "{}", g.log());
    assert!(
        std::fs::read_to_string(g.state.join("revoked_more"))
            .unwrap_or_default()
            .contains(TOKEN2)
    );
    for r in [R1, R2] {
        let at = std::fs::read_to_string(secret.join(format!("token-{r}.expires-at"))).unwrap();
        assert_ne!(at, LIVE, "request {r}'s key still reads live after its act");
    }
    assert!(!g.slot_held(ORG), "the slot outlived the acts");
}

/// The review of c21401da, F1 (blocking): the slot file is per OWNER and a
/// plan keeps it, so request 2 often finds request 1's live token there.
/// When the Secret could not be read, the render KEPT it, and request 2
/// acted with request 1's token and then revoked it — the strand again, on
/// the degraded path. The reviewer's repro-keep.sh, through the verb: the
/// REAL render against an API server that times out, the slot holding
/// another request's live token. Nothing reaches GitHub, and no slot is
/// left.
#[test]
fn a_failed_secret_read_never_acts_with_another_requests_token() {
    needs_tools!();
    const OTHER: &str = "11111111-1111-4111-8111-111111111111";
    let g = Gh::new("keep-sibling");
    write_file(&g.tokens.join(format!("{ORG}.token.request")), OTHER);
    let kubectl = g.root.join("kubectl");
    write_exec(
        &kubectl,
        "#!/bin/sh\necho 'Unable to connect to the server: dial tcp: i/o timeout' >&2\nexit 1\n",
    );
    let render = repo_root().join("infra/forge/credential-render.sh");
    let o = g.run_env(
        &["create-repository", "--plan", ORG, "boss-dr", "private"],
        &[
            ("BOSS_GITHUB_RENDER", &render.display().to_string()),
            ("BOSS_RENDER_KUBECTL", &kubectl.display().to_string()),
        ],
    );
    assert_eq!(
        o.code,
        78,
        "a request acted with another request's token:\n{}",
        o.text()
    );
    assert!(g.log().is_empty(), "GitHub was called: {}", g.log());
    assert!(
        !g.slot_held(ORG),
        "the sibling's token is still in the slot"
    );
}

/// And the verb holds the line itself, whatever left the slot: a slot that
/// does not name THIS request — another's, or none — is refused before
/// GitHub is called.
#[test]
fn a_slot_rendered_for_another_request_is_refused_before_github() {
    needs_tools!();
    for (why, named) in [
        (
            "another request",
            Some("11111111-1111-4111-8111-111111111111"),
        ),
        ("no request", None),
    ] {
        let g = Gh::new("slot-request");
        let f = g.tokens.join(format!("{ORG}.token.request"));
        match named {
            Some(r) => write_file(&f, r),
            None => std::fs::remove_file(&f).unwrap(),
        }
        let o = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
        assert_eq!(o.code, 78, "{why}:\n{}", o.text());
        assert!(o.stderr.contains(REQUEST), "{why}: {}", o.stderr);
        assert!(g.log().is_empty(), "{why}: {}", g.log());
    }
}

#[test]
fn the_token_travels_only_in_a_private_header_file() {
    needs_tools!();
    let g = Gh::new("token-file");
    let o = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(o.code, 0, "{}", o.text());
    let argv = g.argv_log();
    assert!(
        argv.contains("-H @"),
        "curl was not handed a header file: {argv}"
    );
    assert!(
        argv.starts_with("-q "),
        "-q must be curl's first argument, or a .curlrc is read: {argv}"
    );
    assert!(
        argv.contains("--max-redirs 0"),
        "a redirect could be followed: {argv}"
    );
    let body = std::fs::read_to_string(script()).unwrap();
    let code: Vec<&str> = body
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(
        !code
            .iter()
            .any(|l| l.contains("set -x") || l.contains("$HOME")),
        "github-act.sh traces or reads $HOME (the ops runner has none)"
    );
}

/// THE HEADER FILE LIVES IN THE UNIT'S RUNTIME DIRECTORY (backlog
/// 5f77b205). It used to be made under the host's shared /tmp, where a
/// SIGKILL, an OOM kill or a power loss left the token on disk. Now it
/// is made under RUNTIME_DIRECTORY — tmpfs that systemd removes when the
/// run stops — and the script's own trap still empties it on a normal
/// end. Without the variable (a hand run, a unit that lost the line) the
/// script refuses before GitHub is called rather than fall back to /tmp.
#[test]
fn the_token_scratch_is_made_in_the_runtime_directory_or_not_at_all() {
    needs_tools!();
    let g = Gh::new("runtime-dir");
    let o = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(o.code, 0, "{}", o.text());
    let argv = g.argv_log();
    let header = argv
        .split(' ')
        .find_map(|w| w.strip_prefix('@'))
        .unwrap_or_else(|| panic!("curl was not handed a header file: {argv}"));
    let want = format!("{}/github-act.", g.run_dir.display());
    assert!(
        header.starts_with(&want),
        "the token's header file {header} is not under the unit's RUNTIME_DIRECTORY ({want}…)"
    );
    let left: Vec<_> = std::fs::read_dir(&g.run_dir).unwrap().collect();
    assert!(
        left.is_empty(),
        "github-act.sh left {} entr(ies) in the runtime directory after a normal end",
        left.len()
    );

    let g = Gh::new("runtime-dir-unset");
    let out = g
        .cmd(&["create-repository", "--plan", ORG, "boss-dr", "private"])
        .env_remove("RUNTIME_DIRECTORY")
        .output()
        .expect("github-act.sh runs");
    let err = String::from_utf8_lossy(&out.stderr);
    // A REFUSAL (78, "nothing was changed on GitHub"), not a failure (1):
    // nothing has been sent anywhere when the directory is missing.
    assert_eq!(
        out.status.code(),
        Some(78),
        "a missing runtime directory must be refused, not failed: {err}"
    );
    assert!(
        err.contains("nothing was changed on GitHub"),
        "the refusal must say nothing reached GitHub: {err}"
    );
    assert!(
        err.contains("RUNTIME_DIRECTORY") && err.contains("RuntimeDirectory="),
        "the refusal must name the variable and the unit line that sets it: {err}"
    );
    assert!(
        g.log().is_empty(),
        "GitHub was called with no private place for the token: {}",
        g.log()
    );
}

// ---------------------------------------------------------------------------
// set-branch-protection
// ---------------------------------------------------------------------------

fn put_boss_dr(g: &Gh) {
    g.put_repo(
        ORG,
        "boss-dr",
        json!({"id": 9, "full_name": "algedonic-dev/boss-dr", "private": true, "visibility": "private", "fork": false, "archived": false}),
    );
}

#[test]
fn a_ruleset_is_created_on_the_signed_plan_and_read_back_normalised() {
    needs_tools!();
    let g = Gh::new("protect-create");
    put_boss_dr(&g);
    let args = [ORG, "boss-dr", "main", "app:123456", "none"];
    let p = g.run(&[&["set-branch-protection", "--plan"][..], &args[..]].concat());
    contains_all(
        &p.stdout,
        &[
            "plan: github-set-branch-protection",
            "ruleset: boss: refs/heads/main (absent)",
            "current:\n  absent",
            "  bypass: Integration:123456:always",
            "  rule: creation",
            "  rule: deletion",
            "  rule: non_fast_forward",
            "  rule: update fetch_and_merge=false",
            "act: POST /repos/algedonic-dev/boss-dr/rulesets",
        ],
        "the plan",
    );
    let sha = p.plan_sha();
    let w = g.run(
        &[
            &["set-branch-protection"][..],
            &args[..],
            &[sha.as_str()][..],
        ]
        .concat(),
    );
    assert_eq!(w.code, 0, "{}", w.text());
    assert!(w.stdout.contains("proven — ruleset 101"), "{}", w.text());
    let rs = g.rulesets(ORG, "boss-dr");
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0]["name"], "boss: refs/heads/main");
    assert_eq!(
        rs[0]["conditions"]["ref_name"]["include"],
        json!(["refs/heads/main"])
    );

    // Applied, the plan reads "already as desired" — and a write of that
    // plan writes nothing.
    g.remint();
    let p2 = g.run(&[&["set-branch-protection", "--plan"][..], &args[..]].concat());
    contains_all(&p2.stdout, &["(id 101)", "act: none"], "the second plan");
    let w2 = g.run(
        &[
            &["set-branch-protection"][..],
            &args[..],
            &[p2.plan_sha().as_str()][..],
        ]
        .concat(),
    );
    assert_eq!(w2.code, 0, "{}", w2.text());
    assert_eq!(
        g.writes(),
        vec!["POST /repos/algedonic-dev/boss-dr/rulesets".to_string()]
    );
}

#[test]
fn a_changed_allowlist_replaces_the_ruleset_whole_with_required_checks() {
    needs_tools!();
    let g = Gh::new("protect-put");
    put_boss_dr(&g);
    let first = [ORG, "boss-dr", "publish/**", "app:1", "none"];
    let sha = g
        .run(&[&["set-branch-protection", "--plan"][..], &first[..]].concat())
        .plan_sha();
    assert_eq!(
        g.run(
            &[
                &["set-branch-protection"][..],
                &first[..],
                &[sha.as_str()][..]
            ]
            .concat()
        )
        .code,
        0
    );
    g.remint();
    let second = [
        ORG,
        "boss-dr",
        "publish/**",
        "app:1,team:77",
        "build,test/unit",
    ];
    let p = g.run(&[&["set-branch-protection", "--plan"][..], &second[..]].concat());
    contains_all(
        &p.stdout,
        &[
            "ruleset: boss: refs/heads/publish/** (id 101)",
            "  bypass: Integration:1:always\n",
            "  bypass: Integration:1:always Team:77:always",
            "  rule: required_status_checks build@any,test/unit@any strict=false do_not_enforce_on_create=false",
            "act: PUT /repos/algedonic-dev/boss-dr/rulesets/101",
        ],
        "the replacing plan",
    );
    let w = g.run(
        &[
            &["set-branch-protection"][..],
            &second[..],
            &[p.plan_sha().as_str()][..],
        ]
        .concat(),
    );
    assert_eq!(w.code, 0, "{}", w.text());
    assert_eq!(
        g.rulesets(ORG, "boss-dr").len(),
        1,
        "a second ruleset was made"
    );
    assert!(
        g.writes()
            .contains(&"PUT /repos/algedonic-dev/boss-dr/rulesets/101".to_string())
    );
}

#[test]
fn a_ruleset_that_does_not_read_back_fails_and_bad_arguments_are_refused() {
    needs_tools!();
    let g = Gh::new("protect-refuse");
    put_boss_dr(&g);
    let args = [ORG, "boss-dr", "main", "none", "none"];
    let sha = g
        .run(&[&["set-branch-protection", "--plan"][..], &args[..]].concat())
        .plan_sha();
    g.fault("ignore_writes", "");
    let w = g.run(
        &[
            &["set-branch-protection"][..],
            &args[..],
            &[sha.as_str()][..],
        ]
        .concat(),
    );
    assert_eq!(w.code, 1, "{}", w.text());
    assert!(
        w.stderr.contains("does not read back as desired"),
        "{}",
        w.stderr
    );

    let g = Gh::new("protect-args");
    put_boss_dr(&g);
    for (pattern, allow, checks) in [
        ("a//b", "none", "none"),
        ("-x", "none", "none"),
        ("main", "user:5", "none"),
        ("main", "app:1,app:1", "none"),
        ("main", "none", "build,build"),
        ("main", "none", "has space"),
    ] {
        let o = g.run(&[
            "set-branch-protection",
            "--plan",
            ORG,
            "boss-dr",
            pattern,
            allow,
            checks,
        ]);
        assert_eq!(
            o.code,
            78,
            "{pattern} {allow} {checks} was not refused:\n{}",
            o.text()
        );
    }
    // Two rulesets under the verb's one name: it will not guess.
    for id in [201, 202] {
        let d = g.state.join(format!("repos/{ORG}/boss-dr.rulesets"));
        std::fs::create_dir_all(&d).unwrap();
        write_file(
            &d.join(format!("{id}.json")),
            &json!({"id": id, "name": "boss: refs/heads/main", "target": "branch", "enforcement": "active"}).to_string(),
        );
    }
    let o = g.run(&[
        "set-branch-protection",
        "--plan",
        ORG,
        "boss-dr",
        "main",
        "none",
        "none",
    ]);
    assert_eq!(o.code, 78, "{}", o.text());
    assert!(o.stderr.contains("will not guess"), "{}", o.stderr);
    assert!(g.writes().is_empty(), "{:?}", g.writes());
}

// ---------------------------------------------------------------------------
// delete-refs, against a real git repository
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository as GitHub would hold it: default branch `main`,
/// a publish branch, two scratch branches and a tag.
struct Remote {
    repo: PathBuf,
    a: String,
    b: String,
}

fn remote(g: &Gh, owner: &str, name: &str, default: &str) -> Remote {
    let repo = g.github.join(owner).join(format!("{name}.git"));
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "--bare"]);
    let tree = git(&repo, &["hash-object", "-t", "tree", "-w", "--stdin"]);
    let a = git(&repo, &["commit-tree", &tree, "-m", "a"]);
    let b = git(&repo, &["commit-tree", &tree, "-p", &a, "-m", "b"]);
    for (r, s) in [
        ("refs/heads/main", &a),
        ("refs/heads/trunk", &a),
        ("refs/heads/publish/2026-09-27-abc", &b),
        ("refs/heads/scratch/one", &a),
        ("refs/heads/scratch/two", &b),
        ("refs/tags/old", &a),
    ] {
        git(&repo, &["update-ref", r, s]);
    }
    git(
        &repo,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{default}")],
    );
    Remote { repo, a, b }
}

fn refs_of(r: &Remote) -> Vec<String> {
    git(&r.repo, &["for-each-ref", "--format=%(refname)"])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn named_refs_are_deleted_by_one_leased_push_and_read_back_gone() {
    needs_tools!();
    let g = Gh::new("refs-delete");
    let r = remote(&g, ORG, "boss-mirror", "main");
    let args = [
        ORG,
        "boss-mirror",
        "heads/scratch/two,heads/scratch/one,tags/old,heads/gone",
        "none",
    ];
    let p = g.run(&[&["delete-refs", "--plan"][..], &args[..]].concat());
    contains_all(
        &p.stdout,
        &[
            "plan: github-delete-refs",
            "default branch: main",
            "refs named: 4 — to delete: 3, already absent: 1",
            &format!("delete refs/heads/scratch/one at {}", r.a),
            &format!("delete refs/heads/scratch/two at {}", r.b),
            &format!("delete refs/tags/old at {}", r.a),
            "absent refs/heads/gone — nothing to delete",
            "act: one atomic git push of 3 delete(s)",
        ],
        "the plan",
    );
    assert_eq!(refs_of(&r).len(), 6, "a plan deleted something");
    let w = g.run(
        &[
            &["delete-refs"][..],
            &args[..],
            &[p.plan_sha().as_str()][..],
        ]
        .concat(),
    );
    assert_eq!(w.code, 0, "{}", w.text());
    assert!(
        w.stdout.contains("proven — 3 ref(s) deleted"),
        "{}",
        w.text()
    );
    assert_eq!(
        refs_of(&r),
        vec![
            "refs/heads/main".to_string(),
            "refs/heads/publish/2026-09-27-abc".to_string(),
            "refs/heads/trunk".to_string(),
        ]
    );
}

#[test]
fn a_ref_that_moved_after_the_plan_voids_it_and_nothing_is_deleted() {
    needs_tools!();
    let g = Gh::new("refs-moved");
    let r = remote(&g, ORG, "boss-mirror", "main");
    let args = [
        ORG,
        "boss-mirror",
        "heads/scratch/one,heads/scratch/two",
        "none",
    ];
    let sha = g
        .run(&[&["delete-refs", "--plan"][..], &args[..]].concat())
        .plan_sha();
    git(&r.repo, &["update-ref", "refs/heads/scratch/one", &r.b]);
    let w = g.run(&[&["delete-refs"][..], &args[..], &[sha.as_str()][..]].concat());
    assert_eq!(w.code, 78, "{}", w.text());
    assert!(w.stderr.contains("not the approved"), "{}", w.stderr);
    assert_eq!(refs_of(&r).len(), 6, "a voided plan deleted something");
}

#[test]
fn the_default_branch_is_refused_and_main_needs_a_reason() {
    needs_tools!();
    let g = Gh::new("refs-main");
    let r = remote(&g, ORG, "boss-mirror", "trunk");
    let o = g.run(&[
        "delete-refs",
        "--plan",
        ORG,
        "boss-mirror",
        "heads/trunk",
        "moved-to-publish-only",
    ]);
    assert_eq!(o.code, 78, "{}", o.text());
    assert!(o.stderr.contains("DEFAULT branch"), "{}", o.stderr);
    let o = g.run(&[
        "delete-refs",
        "--plan",
        ORG,
        "boss-mirror",
        "heads/main",
        "none",
    ]);
    assert_eq!(o.code, 78, "{}", o.text());
    assert!(o.stderr.contains("with no reason"), "{}", o.stderr);
    let args = [
        ORG,
        "boss-mirror",
        "heads/main",
        "public-fork-carries-no-main",
    ];
    let p = g.run(&[&["delete-refs", "--plan"][..], &args[..]].concat());
    contains_all(
        &p.stdout,
        &[
            "reason: public-fork-carries-no-main",
            "delete refs/heads/main at",
        ],
        "the plan",
    );
    let w = g.run(
        &[
            &["delete-refs"][..],
            &args[..],
            &[p.plan_sha().as_str()][..],
        ]
        .concat(),
    );
    assert_eq!(w.code, 0, "{}", w.text());
    assert!(!refs_of(&r).contains(&"refs/heads/main".to_string()));
    for bad in [
        "heads/a..b",
        "heads/*",
        "refs/heads/x",
        "heads/x,heads/x",
        "pull/1/head",
    ] {
        let o = g.run(&["delete-refs", "--plan", ORG, "boss-mirror", bad, "none"]);
        assert_eq!(o.code, 78, "{bad} was not refused:\n{}", o.text());
    }
}

// ---------------------------------------------------------------------------
// the verb files, and through the ops runner
// ---------------------------------------------------------------------------

const PAIRS: [(&str, &str, &str); 3] = [
    (
        "github-create-repository",
        "plan-a-github-repository",
        "create-repository",
    ),
    (
        "github-set-branch-protection",
        "plan-a-github-branch-protection",
        "set-branch-protection",
    ),
    (
        "github-delete-refs",
        "plan-a-github-ref-delete",
        "delete-refs",
    ),
];

fn verb(name: &str) -> Value {
    let p = repo_root().join(format!("infra/ops/verbs/{name}.json"));
    serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap()
}

fn param_names(v: &Value) -> Vec<String> {
    v["params"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn each_write_is_an_approval_verb_on_the_forge_with_its_read_only_plan() {
    for (write, plan, act) in PAIRS {
        let w = verb(write);
        let p = verb(plan);
        let about = w["about"].as_str().unwrap();
        assert!(
            about.starts_with("MUTATING"),
            "{write} does not say it is MUTATING"
        );
        assert!(about.contains("David"), "{write} names no authorization");
        assert!(
            p["about"].as_str().unwrap().starts_with("READ-ONLY"),
            "{plan}"
        );
        assert_eq!(w["requires_approval"], true, "{write}");
        assert_eq!(w["plan_verb"], plan, "{write}");
        assert_eq!(w["approvers"], json!([APPROVER]), "{write}");
        assert_ne!(p["requires_approval"], true, "{plan} is read-only");
        assert_eq!(w["hosts"], json!(["forge"]), "{write} serves the forge");
        assert_eq!(p["hosts"], w["hosts"], "{plan} serves {write}'s host");
        let mut wn = param_names(&w);
        assert_eq!(wn.pop().as_deref(), Some("plan_sha256"), "{write}");
        assert_eq!(
            wn,
            param_names(&p),
            "{plan} renders from exactly {write}'s args"
        );
        assert_eq!(
            wn[0], "owner",
            "{write}: the installation is the request's owner"
        );
        let n = wn.len();
        let placeholders = |from: usize, to: usize| -> Vec<Value> {
            (from..=to).map(|i| json!(format!("{{{i}}}"))).collect()
        };
        let mut want_w = vec![json!("infra/forge/github-act.sh"), json!(act)];
        want_w.extend(placeholders(1, n + 1));
        assert_eq!(w["argv"], json!(want_w), "{write}'s argv");
        let mut want_p = vec![
            json!("infra/forge/github-act.sh"),
            json!(act),
            json!("--plan"),
        ];
        want_p.extend(placeholders(1, n));
        assert_eq!(p["argv"], json!(want_p), "{plan}'s argv");
        assert!(
            !wn.iter()
                .any(|n| n.contains("token") || n.contains("path") || n.contains("file")),
            "{write} takes a credential or a path from the packet: {wn:?}"
        );
    }
    let mode = std::fs::metadata(script()).unwrap().permissions().mode();
    assert!(mode & 0o111 != 0, "github-act.sh is not executable");
}

/// The allowlist's patterns admit the values the exercises need and
/// refuse whitespace, a leading dash and a traversal.
#[test]
fn the_patterns_admit_the_exercises_and_nothing_shaped_like_an_option() {
    let pat = |v: &str, name: &str| -> String {
        verb(v)["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .and_then(|p| p["pattern"].as_str())
            .unwrap_or_else(|| panic!("{v}.{name} has no pattern"))
            .to_string()
    };
    let matches = |p: &str, s: &str| -> bool {
        Command::new("jq")
            .args([
                "-n",
                "-e",
                "--arg",
                "p",
                p,
                "--arg",
                "s",
                s,
                "$s | test($p)",
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !has("jq") {
        eprintln!("skipping: the runner matches patterns with jq, and this box has none");
        return;
    }
    let good: &[(&str, &str, &str)] = &[
        ("github-create-repository", "owner", "algedonic-dev"),
        ("github-create-repository", "name", "boss-dr"),
        ("github-set-branch-protection", "pattern", "publish/**"),
        ("github-set-branch-protection", "push_allow", "app:123456"),
        ("github-set-branch-protection", "push_allow", "none"),
        ("github-set-branch-protection", "checks", "none"),
        ("github-set-branch-protection", "checks", "build,test/unit"),
        ("github-delete-refs", "owner", "dauld"),
        ("github-delete-refs", "repo", "boss-mirror"),
        (
            "github-delete-refs",
            "refs",
            "heads/main,heads/scratch/one,tags/old",
        ),
        (
            "github-delete-refs",
            "reason",
            "public-fork-carries-no-main",
        ),
        ("github-delete-refs", "reason", "none"),
    ];
    for (v, n, s) in good {
        assert!(matches(&pat(v, n), s), "{v}.{n} refuses {s}");
    }
    for (write, _, _) in PAIRS {
        for p in verb(write)["params"].as_array().unwrap() {
            let Some(pattern) = p["pattern"].as_str() else {
                continue;
            };
            for bad in ["-x", "a b", "a\tb", "../x", ""] {
                assert!(
                    !matches(pattern, bad),
                    "{write}.{} admits {bad:?}",
                    p["name"]
                );
            }
        }
    }
    let vis = &verb("github-create-repository")["params"][2];
    assert_eq!(
        vis["one_of"],
        json!(["private", "public"]),
        "visibility is a reviewed literal"
    );
}

fn run_runner(g: &Gh, verb: &str, args: Value) -> (String, Option<Value>) {
    let bin = g.root.join("rbin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("curl"),
        &[
            "#!/bin/sh\n",
            boss_testing::ops_runner_stub::RECORD_STEP_METADATA,
            "cat \"$STUB_JOBS\"\n",
        ]
        .concat(),
    );
    let verbs = g.root.join("verbs");
    std::fs::create_dir_all(&verbs).unwrap();
    for e in std::fs::read_dir(repo_root().join("infra/ops/verbs")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            std::fs::copy(&p, verbs.join(p.file_name().unwrap())).unwrap();
        }
    }
    let jobs = g.root.join("jobs.json");
    write_file(
        &jobs,
        &json!({"data": [{
            "id": "aaaaaaaa-0000-4000-8000-000000000000", "status": "open",
            "metadata": {"host": "forge", "verb": verb, "args": args},
            "steps": [{"id": "s-execute", "spec_slug": "execute", "status": "ready",
                       "metadata": {"authority_role": "platform-admin"}}]
        }]})
        .to_string(),
    );
    let step_md = g.root.join("step-metadata.json");
    let _ = std::fs::remove_file(&step_md);
    // The slot as the render leaves it for the request the runner answers.
    write_file(
        &g.tokens.join(format!("{ORG}.token.request")),
        "aaaaaaaa-0000-4000-8000-000000000000",
    );
    // The script's own seams (its stub GitHub, its token slots) ride the
    // runner's environment, as the forge unit's would — but not the
    // request id: the forge unit carries none, and the verb must be handed
    // the packet it answers by the runner itself (backlog 4ce4ec55).
    let cmd = g.cmd(&[]);
    let out = Command::new("sh")
        .arg(repo_root().join("infra/ops/ops-runner.sh"))
        .env_clear()
        .envs(
            cmd.get_envs()
                .filter(|(k, _)| *k != "OPS_REQUEST_ID")
                .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned()))),
        )
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOST_ID", "forge")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", &verbs)
        .env("STUB_JOBS", &jobs)
        .env("STUB_STEP_METADATA", &step_md)
        .output()
        .expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        text,
        boss_testing::ops_runner_stub::step_metadata_written(&step_md),
    )
}

#[test]
fn through_the_runner_the_plan_is_answered_and_an_unapproved_write_never_reaches_github() {
    needs_tools!();
    let g = Gh::new("runner");
    let (text, meta) = run_runner(
        &g,
        "plan-a-github-repository",
        json!([ORG, "boss-dr", "private"]),
    );
    let meta = meta.unwrap_or_else(|| panic!("the runner completed no step:\n{text}"));
    assert_eq!(meta["disposition"], "answered", "{text}");
    let output = meta["output"].as_str().unwrap_or_default();
    contains_all(
        output,
        &["act: POST /orgs/algedonic-dev/repos", "plan-sha256:"],
        "the packet's output",
    );
    assert!(g.writes().is_empty(), "a plan verb wrote: {:?}", g.writes());
    assert!(
        !g.renders().is_empty()
            && g.renders()
                .iter()
                .all(|r| r.ends_with("--request aaaaaaaa-0000-4000-8000-000000000000")),
        "the runner hands the verb the request it answers, and the verb renders that \
         request's own token: {:?}",
        g.renders()
    );

    let g = Gh::new("runner-write");
    let (text, meta) = run_runner(
        &g,
        "github-create-repository",
        json!([ORG, "boss-dr", "private", "0".repeat(64)]),
    );
    let meta = meta.unwrap_or_else(|| panic!("the runner completed no step:\n{text}"));
    assert_eq!(
        meta["disposition"], "refused",
        "an unapproved write was not refused:\n{text}"
    );
    assert!(
        g.log().is_empty(),
        "an unapproved write reached GitHub: {}",
        g.log()
    );
    assert!(!text.contains(TOKEN));
}

// ---------------------------------------------------------------------------
// The adversarial review of 78959555 (RELEASE-WITH-FIXES): M1, M2, L1-L4,
// L6-L9, each pinned here.
// ---------------------------------------------------------------------------

/// M1. The plan-time read of the verb's own ruleset must BE that ruleset:
/// an empty or `null` 200, or another ruleset's body, rendered a signable
/// plan with an empty `current:` and a PUT.
#[test]
fn a_ruleset_answer_that_is_not_that_ruleset_renders_no_plan() {
    needs_tools!();
    let g = Gh::new("ruleset-body");
    put_boss_dr(&g);
    let args = [ORG, "boss-dr", "main", "app:1", "none"];
    let sha = g
        .run(&[&["set-branch-protection", "--plan"][..], &args[..]].concat())
        .plan_sha();
    let w = g.run(
        &[
            &["set-branch-protection"][..],
            &args[..],
            &[sha.as_str()][..],
        ]
        .concat(),
    );
    assert_eq!(w.code, 0, "{}", w.text());
    g.remint();
    for body in [
        "",
        "null",
        r#"{"id": 999, "name": "boss: refs/heads/main", "target": "branch"}"#,
        r#"{"id": 101, "name": "someone else's", "target": "branch"}"#,
    ] {
        g.fault("ruleset_body", body);
        let p = g.run(&[&["set-branch-protection", "--plan"][..], &args[..]].concat());
        assert_eq!(p.code, 1, "{body:?} rendered a plan:\n{}", p.text());
        assert!(
            p.stderr.contains("is not ruleset 101"),
            "{body:?}: {}",
            p.stderr
        );
    }
    assert_eq!(g.writes().len(), 1, "{:?}", g.writes());
}

/// L2. A required check pinned to another App reads as drift, not as done.
#[test]
fn a_check_pinned_to_another_integration_does_not_read_back() {
    needs_tools!();
    let g = Gh::new("ruleset-integration");
    put_boss_dr(&g);
    let args = [ORG, "boss-dr", "main", "none", "build"];
    let sha = g
        .run(&[&["set-branch-protection", "--plan"][..], &args[..]].concat())
        .plan_sha();
    g.fault("ruleset_integration", "");
    let w = g.run(
        &[
            &["set-branch-protection"][..],
            &args[..],
            &[sha.as_str()][..],
        ]
        .concat(),
    );
    assert_eq!(w.code, 1, "{}", w.text());
    assert!(w.stderr.contains("build@999"), "{}", w.stderr);
}

/// L8. The read-back and the exists check compare visibility and archive,
/// not only `private`.
#[test]
fn a_repository_of_another_visibility_or_archived_does_not_pass() {
    needs_tools!();
    for (i, over) in [json!({"visibility": "internal"}), json!({"archived": true})]
        .iter()
        .enumerate()
    {
        let g = Gh::new(&format!("create-as-{i}"));
        let sha = g
            .run(&["create-repository", "--plan", ORG, "boss-dr", "private"])
            .plan_sha();
        g.fault("create_as", &over.to_string());
        let w = g.run(&["create-repository", ORG, "boss-dr", "private", &sha]);
        assert_eq!(w.code, 1, "{over} read back as created:\n{}", w.text());
        assert!(w.stderr.contains("does not read back"), "{}", w.stderr);
    }
    let g = Gh::new("exists-internal");
    g.put_repo(
        ORG,
        "boss-dr",
        json!({"id": 9, "full_name": "algedonic-dev/boss-dr", "private": true, "visibility": "internal", "fork": false, "archived": false}),
    );
    let p = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(p.code, 78, "{}", p.text());
    assert!(p.stderr.contains("visibility internal"), "{}", p.stderr);
}

/// L9. A repository that lists refs but advertises no HEAD cannot say
/// which branch is the default, so nothing is planned.
#[test]
fn a_listing_with_refs_and_no_head_is_refused() {
    needs_tools!();
    let g = Gh::new("refs-no-head");
    let r = remote(&g, ORG, "boss-mirror", "not-a-branch");
    let o = g.run(&[
        "delete-refs",
        "--plan",
        ORG,
        "boss-mirror",
        "heads/scratch/one",
        "none",
    ]);
    assert_eq!(o.code, 78, "{}", o.text());
    assert!(o.stderr.contains("advertises no HEAD"), "{}", o.stderr);
    assert_eq!(refs_of(&r).len(), 6);
}

/// L3. A token with under five minutes left cannot outlive a 120 s verb
/// and its read-backs; it is refused as expiring.
#[test]
fn a_token_within_five_minutes_of_expiry_is_refused() {
    needs_tools!();
    let soon = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 200;
    let at = Command::new("date")
        .args(["-u", "-d", &format!("@{soon}"), "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    let at = String::from_utf8_lossy(&at.stdout).trim().to_string();
    let g = Gh::new("slot-soon");
    g.slot(ORG, TOKEN, Some(&at));
    let o = g.run(&["create-repository", "--plan", ORG, "boss-dr", "private"]);
    assert_eq!(o.code, 78, "{}", o.text());
    assert!(o.stderr.contains("expires within"), "{}", o.stderr);
    assert!(g.log().is_empty(), "{}", g.log());
}

/// L4. An environment that turns tracing on cannot print the token, and
/// without the test marker every BOSS_GITHUB_* seam is ignored.
#[test]
fn an_inherited_trace_prints_no_token_and_seams_need_the_marker() {
    needs_tools!();
    let g = Gh::new("xtrace");
    let o = g.run_env(
        &["create-repository", "--plan", ORG, "boss-dr", "private"],
        &[("SHELLOPTS", "xtrace"), ("BASH_XTRACEFD", "2")],
    );
    assert_eq!(o.code, 0, "{}", o.text()); // run_env asserts no token anywhere
    let g = Gh::new("no-marker");
    let o = g.run_env(
        &["create-repository", "--plan", ORG, "boss-dr", "private"],
        &[("BOSS_GITHUB_ACT_SEAMS", "")],
    );
    assert_eq!(o.code, 78, "{}", o.text());
    assert!(
        o.stderr
            .contains("/etc/boss-publish/github-app/algedonic-dev.token"),
        "the seam was honoured without the marker: {}",
        o.stderr
    );
    assert!(g.log().is_empty(), "{}", g.log());
    assert!(
        g.renders().is_empty(),
        "the render seam was honoured without the marker: {:?}",
        g.renders()
    );
}

/// L7. The credential helper git runs answers ONLY https://github.com.
#[test]
fn the_credential_helper_answers_github_com_over_https_only() {
    needs_tools!();
    let g = Gh::new("helper");
    let copy = g.root.join("token-copy");
    write_file(&copy, TOKEN);
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600)).unwrap();
    let fill = |protocol: &str, host: &str| -> String {
        let mut c = Command::new("git");
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "credential.helper=", "-c"])
            .arg(format!(
                "credential.helper=!bash '{}' credential-helper '{}'",
                script().display(),
                copy.display()
            ))
            .args(["credential", "fill"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        boss_testing::feed_stdin(
            &mut child,
            format!("protocol={protocol}\nhost={host}\n\n").as_bytes(),
        );
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let ok = fill("https", "github.com");
    assert!(
        ok.contains(&format!("password={TOKEN}")) && ok.contains("username=x-access-token"),
        "https://github.com got no credential: {ok}"
    );
    for (p, h) in [
        ("https", "evil.example"),
        ("https", "github.com.evil.example"),
        ("http", "github.com"),
        ("https", "api.github.com"),
    ] {
        assert!(!fill(p, h).contains(TOKEN), "the helper answered {p}://{h}");
    }
    let body = std::fs::read_to_string(script()).unwrap();
    let helper_line = body
        .lines()
        .find(|l| l.contains("credential.helper=!"))
        .expect("github-act.sh configures its helper");
    assert!(
        helper_line.contains("credential-helper") && helper_line.contains("$WORK/token"),
        "the helper must read the private copy in $WORK, never the slot (L6): {helper_line}"
    );
    assert!(
        body.contains("-c http.followRedirects=false"),
        "git may follow a redirect with the token (L1)"
    );
}

/// M2. The lint finds a verb that reaches github-act.sh by ANY spelling —
/// a `./`, a `..`, a wrapping interpreter — and holds it to the rule; a
/// verb naming an act the script does not own is refused too.
#[test]
fn the_lint_finds_a_github_verb_however_its_script_is_spelled() {
    needs_tools!();
    if !has("python3") {
        eprintln!("skipping: the lint is python3");
        return;
    }
    let lint = |name: &str, extra: Option<Value>| -> (i32, String) {
        let root = scratch_dir(&format!("github-act-lint-{name}"));
        let verbs = root.join("verbs");
        std::fs::create_dir_all(&verbs).unwrap();
        for e in std::fs::read_dir(repo_root().join("infra/ops/verbs")).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "json") {
                std::fs::copy(&p, verbs.join(p.file_name().unwrap())).unwrap();
            }
        }
        if let Some(v) = extra {
            write_file(&verbs.join("sneaky.json"), &v.to_string());
        }
        let out = Command::new("bash")
            .arg(repo_root().join("infra/lint/the-controls-are-bounded-verbs.sh"))
            .env("BOUNDED_VERBS_DIR", &verbs)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    let (code, text) = lint("control", None);
    assert_eq!(code, 0, "the shipped verbs fail the lint:\n{text}");
    let sneaky = |argv: Value| {
        json!({
            "about": "READ-ONLY: a verb that is not what it says",
            "hosts": ["forge"],
            "argv": argv,
            "params": [
                {"name": "owner", "pattern": "^[a-z]+$"},
                {"name": "name", "pattern": "^[a-z]+$"},
                {"name": "visibility", "one_of": ["private"]},
                {"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}
            ]
        })
    };
    for (i, argv) in [
        json!([
            "infra/forge/./github-act.sh",
            "create-repository",
            "{1}",
            "{2}",
            "{3}",
            "{4}"
        ]),
        json!([
            "infra/ops/../forge/github-act.sh",
            "create-repository",
            "{1}",
            "{2}",
            "{3}",
            "{4}"
        ]),
        json!([
            "bash",
            "infra/forge/github-act.sh",
            "create-repository",
            "{1}",
            "{2}",
            "{3}",
            "{4}"
        ]),
        json!([
            "infra/forge/github-act.sh",
            "credential-helper",
            "--plan",
            "{1}",
            "{2}"
        ]),
    ]
    .into_iter()
    .enumerate()
    {
        let (code, text) = lint(&format!("sneaky-{i}"), Some(sneaky(argv.clone())));
        assert_eq!(code, 1, "{argv} passed the lint:\n{text}");
        assert!(
            text.contains("sneaky"),
            "the refusal does not name the verb: {text}"
        );
    }
}
