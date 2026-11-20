//! `infra/forge/forge-account.sh` — the forge converge creates and
//! verifies each non-admin forge account `infra/forge/forge-accounts.json`
//! declares, the way probe-account.sh already makes and verifies
//! `boss-probe` (design 0af333ee, resolved by David 2026-11-13: question
//! `create`; backlog 70555ffd).
//!
//! Every forge token the estate minted was a token of user david, a
//! Forgejo site admin, and Forgejo does not narrow a site admin's token to
//! the repositories it collaborates on — so scope could not narrow it, and
//! only the principal can. The first account is `boss-reader`, a read
//! collaborator on david/boss and nothing else, which holds the token root
//! on boss-gcp fetches its tree with.
//!
//! Pinned here, against a stub docker (`forgejo admin user list|create`
//! inside the container) and a stub curl (the repository API, read and
//! written with the converge's forge header):
//!   * absent -> created NOT admin, must-change-password off, with a random
//!     password nobody is shown; then the one grant; then verified
//!   * already as declared -> nothing written
//!   * a site admin, or a grant on any other repository -> RED, named, on a
//!     line the converge's judge reads; never repaired by guess
//!   * a wider grant on the declared repository is put back and read back;
//!     a write that answers and does not take is a failure
//!   * no credential, docker unreachable, or a listing that cannot see the
//!     declared repository: cannot answer (exit 4), never a pass
//!   * a listing that shows the declared repository but not every one the
//!     forge's own inventory holds, or an inventory that cannot be read:
//!     cannot answer too (review eb947a26, B1)
//!   * the token and the generated password reach no output
//!   * every broker rule's forge_user is david or a declared account, and
//!     the root-tree token's is boss-reader (the decision)

use boss_testing::{dispatcher_rules_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const TOKEN: &str = "fj-token-must-never-print-70555ffd";
const PASSWORD: &str = "PW-generated-must-never-print-0af333ee";
const ACCOUNT: &str = "boss-reader";
const REPO: &str = "david/boss";

fn script() -> PathBuf {
    repo_root().join("infra/forge/forge-account.sh")
}

fn declaration() -> serde_json::Value {
    let text = std::fs::read_to_string(repo_root().join("infra/forge/forge-accounts.json"))
        .expect("infra/forge/forge-accounts.json is the declaration");
    serde_json::from_str(&text).expect("the declaration is JSON")
}

/// The stub docker. The script calls it in TWO shapes:
///   docker --host unix:///var/run/docker.sock exec -u git forgejo forgejo admin user <list|create> …
///   docker --host unix:///var/run/docker.sock exec -u git forgejo find /data/git/repositories …
/// `$STUB_DIR/users` holds `name email is_admin` lines; `list` prints them
/// under Forgejo's header (ID Username Email IsActive IsAdmin 2FA) and
/// `create` appends one, printing the generated password the way
/// `--random-password` does. `find` prints the forge's own repository
/// directories, lowercased as Forgejo stores them and each with a wiki
/// beside it: `$STUB_DIR/disk.json` when present, else every repository in
/// `repos.json` — the forge's whole inventory, whatever the API's caller
/// may see. `$STUB_DIR/docker_fail` makes the daemon unreachable;
/// `$STUB_DIR/create_ignored` answers a create and adds nothing;
/// `$STUB_DIR/inventory_fail` refuses the `find`.
fn write_docker_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("docker");
    write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
echo "docker $*" >> "$STUB_LOG"
echo '=== call ===' >> "$STUB_LOG"
[ -e "$STUB_DIR/docker_fail" ] && { echo "Cannot connect to the Docker daemon" >&2; exit 1; }
[ "${1:-}" = "--host" ] && [ "${2:-}" = "unix:///var/run/docker.sock" ] || { echo "stub: not the system daemon" >&2; exit 9; }
shift 2
if [ "$1 $2 $3 $4 $5" = "exec -u git forgejo find" ]; then
    shift 5
    [ -e "$STUB_DIR/inventory_fail" ] && { echo "find: /data/git/repositories: No such file or directory" >&2; exit 1; }
    [ "$*" = "/data/git/repositories -mindepth 2 -maxdepth 2 -type d -name *.git" ] || { echo "stub: unexpected find argv $*" >&2; exit 9; }
    disk="$STUB_DIR/disk.json"; [ -e "$disk" ] || disk="$STUB_DIR/repos.json"
    jq -r '.[] | .full_name | ascii_downcase | "/data/git/repositories/\(.).git", "/data/git/repositories/\(.).wiki.git"' "$disk"
    exit 0
fi
[ "$1 $2 $3 $4 $5 $6 $7" = "exec -u git forgejo forgejo admin user" ] || { echo "stub: unexpected argv $*" >&2; exit 9; }
shift 7
users="$STUB_DIR/users"; touch "$users"
case "$1" in
    list)
        printf 'ID\tUsername\tEmail\tIsActive\tIsAdmin\t2FA\n'
        n=0
        while read -r name email admin; do
            n=$((n + 1)); printf '%s\t%s\t%s\ttrue\t%s\tfalse\n' "$n" "$name" "$email" "$admin"
        done < "$users" ;;
    create)
        shift; name=""; email=""; admin=false
        while [ $# -gt 0 ]; do
            case "$1" in
                --username) name="$2"; shift 2 ;;
                --email) email="$2"; shift 2 ;;
                --admin) admin=true; shift ;;
                *) shift ;;
            esac
        done
        grep -q "^$name " "$users" && { echo "user already exists [name: $name]" >&2; exit 1; }
        [ -e "$STUB_DIR/create_ignored" ] || echo "$name $email $admin" >> "$users"
        echo "generated random password is 'PW-generated-must-never-print-0af333ee'"
        echo "New user '$name' has been successfully created!" ;;
    *) echo "stub: unknown verb $1" >&2; exit 9 ;;
esac
"#,
    );
    stub
}

/// The stub curl, in the one shape the script calls it:
///   curl -sS -m <s> -o <out> -w %{http_code} -H @<auth> -H <ct> -X <M> [--data-binary @<body>] <url>
/// `$STUB_DIR/repos.json` is every repository the forge lists
/// (`[{full_name, private}]`), paged by `page`/`limit` like
/// /repos/search; `$STUB_DIR/grants.json` is the account's direct
/// collaborations (`{"owner/name": "read"}`). The permission a repository
/// answers is the grant, else `read` on a public one, else `none`; a
/// repository the account owns answers `owner`. `$STUB_DIR/listed.json`,
/// when present, is what /repos/search shows INSTEAD — the view of a
/// caller that cannot see every repository (review eb947a26, B1) — while
/// the permission routes still answer from `repos.json`. Faults:
/// `curl_fail`, `status_PUT`, `ignore_writes`.
fn write_curl_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("curl");
    write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
out=""; method=GET; body=""; url=""; auth=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -X) method="$2"; shift 2 ;;
        -H) case "$2" in @*) auth="${2#@}" ;; esac; shift 2 ;;
        --data-binary) body="${2#@}"; shift 2 ;;
        -m|-w) shift 2 ;;
        -*) shift ;;
        *) url="$1"; shift ;;
    esac
done
{ echo "$method $url"; [ -n "$body" ] && cat "$body"; echo; echo '=== call ==='; } >> "$STUB_LOG"
[ -e "$STUB_DIR/curl_fail" ] && { echo "curl: (7) Failed to connect to forge port 3000" >&2; exit 7; }
grep -q '^Authorization: token ' "$auth" || { echo '{"message":"no token"}' > "$out"; printf 401; exit 0; }
if [ -e "$STUB_DIR/status_$method" ]; then
    echo '{"message":"stub refusal"}' > "$out"; cat "$STUB_DIR/status_$method"; exit 0
fi
grants="$STUB_DIR/grants.json"; [ -e "$grants" ] || echo '{}' > "$grants"
repos="$STUB_DIR/repos.json"
: > "$out"
case "$method $url" in
    "GET "*/api/v1/repos/search\?*)
        page=$(printf '%s' "$url" | sed -n 's/.*[?&]page=\([0-9]*\).*/\1/p')
        limit=$(printf '%s' "$url" | sed -n 's/.*[?&]limit=\([0-9]*\).*/\1/p')
        listed="$STUB_DIR/listed.json"; [ -e "$listed" ] || listed="$repos"
        jq --argjson p "$page" --argjson l "$limit" '{ok: true, data: .[(($p - 1) * $l):($p * $l)]}' "$listed" > "$out"
        printf 200 ;;
    "GET "*/collaborators/*/permission)
        repo=$(printf '%s' "$url" | sed -E 's#.*/api/v1/repos/([^/]+/[^/]+)/collaborators/.*#\1#')
        user=$(printf '%s' "$url" | sed -E 's#.*/collaborators/([^/]+)/permission$#\1#')
        jq --arg r "$repo" --arg u "$user" --slurpfile g "$grants" '
            (map(select(.full_name == $r)) | first) as $repo
            | if $repo == null then {message: "not found"}
              elif ($r | startswith($u + "/")) then {permission: "owner"}
              else {permission: ($g[0][$r] // (if $repo.private then "none" else "read" end))} end' "$repos" > "$out"
        if jq -e '.permission' "$out" >/dev/null; then printf 200; else printf 404; fi ;;
    "GET "*/collaborators/*)
        repo=$(printf '%s' "$url" | sed -E 's#.*/api/v1/repos/([^/]+/[^/]+)/collaborators/.*#\1#')
        if jq -e --arg r "$repo" 'has($r)' "$grants" >/dev/null; then printf 204; else printf 404; fi ;;
    "PUT "*/collaborators/*)
        repo=$(printf '%s' "$url" | sed -E 's#.*/api/v1/repos/([^/]+/[^/]+)/collaborators/.*#\1#')
        if [ ! -e "$STUB_DIR/ignore_writes" ]; then
            jq --arg r "$repo" --slurpfile b "$body" '. + {($r): $b[0].permission}' "$grants" > "$grants.new" && mv "$grants.new" "$grants"
        fi
        printf 204 ;;
    *) echo '{"message":"stub: no such route"}' > "$out"; printf 404 ;;
esac
"#,
    );
    stub
}

struct Fixture {
    /// `name email is_admin` lines already on the forge.
    users: Vec<&'static str>,
    repos: serde_json::Value,
    /// What /repos/search shows, when it is not every repository.
    listed: Option<serde_json::Value>,
    /// The forge's own repository directories, when they are not `repos`.
    disk: Option<serde_json::Value>,
    grants: serde_json::Value,
    faults: Vec<&'static str>,
    put_status: Option<&'static str>,
    no_auth: bool,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            users: vec!["david david@example.invalid true"],
            repos: serde_json::json!([
                {"full_name": "david/boss", "private": true},
                {"full_name": "david/boss-instance", "private": true},
                {"full_name": "david/public-notes", "private": false},
            ]),
            listed: None,
            disk: None,
            grants: serde_json::json!({}),
            faults: vec![],
            put_status: None,
            no_auth: false,
        }
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    log: String,
    users: String,
    grants: serde_json::Value,
    summary: serde_json::Value,
}

impl Run {
    fn text(&self) -> String {
        format!(
            "exit {:?}\n--- stdout\n{}\n--- stderr\n{}\n--- calls\n{}\n--- summary\n{}",
            self.code, self.stdout, self.stderr, self.log, self.summary
        )
    }
    fn calls(&self) -> Vec<String> {
        self.log
            .split("=== call ===\n")
            .filter_map(|c| c.lines().next().map(str::to_string))
            .filter(|c| !c.is_empty())
            .collect()
    }
    fn creates(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("docker ") && c.contains(" admin user create "))
            .collect()
    }
    fn puts(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("PUT "))
            .collect()
    }
    fn line(&self) -> String {
        self.summary["forge_accounts"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
    fn red(&self) -> String {
        self.summary["forge_account_red"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
    /// Neither the forge token nor the password `--random-password`
    /// printed reaches anything a reader sees.
    fn nothing_secret_printed(&self) {
        for (what, text) in [
            ("stdout", &self.stdout),
            ("stderr", &self.stderr),
            ("summary", &self.summary.to_string()),
        ] {
            assert!(!text.contains(TOKEN), "the token reached {what}:\n{text}");
            assert!(
                !text.contains(PASSWORD),
                "the generated password reached {what}:\n{text}"
            );
        }
        assert!(
            !self.log.contains(TOKEN),
            "the token reached an argv:\n{}",
            self.log
        );
    }
}

fn run(case: &str, f: Fixture) -> Run {
    let dir = scratch_dir(&format!("forge-account-{case}"));
    let docker = write_docker_stub(&dir);
    let curl = write_curl_stub(&dir);
    let state = dir.join("state");
    boss_testing::create_dir(&state);
    let users: String = f.users.iter().map(|u| format!("{u}\n")).collect();
    write_file(&state.join("users"), &users);
    write_file(&state.join("repos.json"), &f.repos.to_string());
    write_file(&state.join("grants.json"), &f.grants.to_string());
    if let Some(listed) = &f.listed {
        write_file(&state.join("listed.json"), &listed.to_string());
    }
    if let Some(disk) = &f.disk {
        write_file(&state.join("disk.json"), &disk.to_string());
    }
    for fault in &f.faults {
        write_file(&state.join(fault), "");
    }
    if let Some(status) = f.put_status {
        write_file(&state.join("status_PUT"), status);
    }
    let auth = dir.join("auth-header");
    if !f.no_auth {
        write_file(&auth, &format!("Authorization: token {TOKEN}\n"));
    }
    let log = dir.join("calls.log");
    write_file(&log, "");
    let summary = dir.join("summary.json");
    let out = Command::new("bash")
        .arg(script())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_FORGE_ACCOUNT_DOCKER", &docker)
        .env("BOSS_FORGE_ACCOUNT_CURL", &curl)
        .env("BOSS_FORGE_URL", "http://forge.test:3000")
        .env("BOSS_FORGE_AUTH_HEADER_FILE", &auth)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("STUB_LOG", &log)
        .env("STUB_DIR", &state)
        .output()
        .expect("bash runs");
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        log: read(&log),
        users: read(&state.join("users")),
        grants: serde_json::from_str(&read(&state.join("grants.json")))
            .unwrap_or(serde_json::Value::Null),
        summary: serde_json::from_str(&read(&summary)).unwrap_or(serde_json::Value::Null),
    }
}

/// The one grant the declaration asks for, as the stub's ledger holds it.
fn granted() -> serde_json::Value {
    serde_json::json!({ REPO: "read" })
}

#[test]
fn an_absent_account_is_created_not_admin_and_granted_read_on_its_one_repository() {
    let r = run("absent", Fixture::default());
    assert_eq!(r.code, Some(0), "{}", r.text());
    let creates = r.creates();
    assert_eq!(creates.len(), 1, "one create: {}", r.text());
    let create = &creates[0];
    for want in [
        "--username boss-reader",
        "--email boss-reader@forge.invalid",
        "--random-password",
        "--must-change-password=false",
    ] {
        assert!(create.contains(want), "the create carries {want}: {create}");
    }
    assert!(
        !create.contains("--admin"),
        "the account is never created a site admin: {create}"
    );
    assert!(
        r.users
            .lines()
            .any(|l| l == "boss-reader boss-reader@forge.invalid false"),
        "{}",
        r.text()
    );
    assert_eq!(
        r.puts(),
        vec![format!(
            "PUT http://forge.test:3000/api/v1/repos/{REPO}/collaborators/{ACCOUNT}"
        )],
        "{}",
        r.text()
    );
    assert!(
        r.log.contains(r#"{"permission":"read"}"#),
        "the grant asks for read: {}",
        r.log
    );
    assert_eq!(r.grants, granted(), "{}", r.text());
    let line = r.line();
    for want in [
        "boss-reader: created and verified",
        "is_admin false",
        "one repository grant: david/boss read",
        "3 repositories read",
    ] {
        assert!(line.contains(want), "the packet line says {want:?}: {line}");
    }
    assert_eq!(r.red(), "", "no red line on a verified account");
    r.nothing_secret_printed();
}

#[test]
fn an_account_already_as_declared_is_not_written() {
    let r = run(
        "already",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid false",
            ],
            grants: serde_json::json!({ REPO: "read" }),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}", r.text());
    assert!(r.creates().is_empty(), "no create: {}", r.text());
    assert!(r.puts().is_empty(), "no write: {}", r.text());
    assert!(
        r.line().contains("boss-reader: verified")
            && r.line().contains("one repository grant: david/boss read"),
        "{}",
        r.line()
    );
    r.nothing_secret_printed();
}

#[test]
fn a_site_admin_account_reds_the_converge_and_is_not_granted() {
    let r = run(
        "admin",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid true",
            ],
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(r.puts().is_empty(), "no grant to an admin: {}", r.text());
    assert!(
        r.red()
            .starts_with("RED forge-converge/forge-account refused: boss-reader is a site admin"),
        "a standing red, in the judge's shape: {}",
        r.text()
    );
    assert!(r.line().contains("boss-reader: RED"), "{}", r.line());
    r.nothing_secret_printed();
}

#[test]
fn a_grant_on_another_private_repository_reds_the_converge_and_names_it() {
    let r = run(
        "other-private",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid false",
            ],
            grants: serde_json::json!({ REPO: "read", "david/boss-instance": "read" }),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(
        r.red()
            .starts_with("RED forge-converge/forge-account refused: ")
            && r.red().contains("david/boss-instance"),
        "{}",
        r.text()
    );
    assert_eq!(
        r.grants["david/boss-instance"], "read",
        "the converge reds; it never removes a grant on a guess"
    );
}

/// A public repository answers `read` to every account, which is not a
/// grant — but a COLLABORATION on one is, and it is told apart by asking.
#[test]
fn a_collaboration_on_a_public_repository_is_a_grant_and_reds() {
    let r = run(
        "other-public",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid false",
            ],
            grants: serde_json::json!({ REPO: "read", "david/public-notes": "read" }),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(r.red().contains("david/public-notes"), "{}", r.text());
}

#[test]
fn a_repository_the_account_owns_reds_the_converge() {
    let mut repos = Fixture::default().repos;
    repos
        .as_array_mut()
        .expect("an array")
        .push(serde_json::json!({"full_name": "boss-reader/scratch", "private": true}));
    let r = run(
        "owns",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid false",
            ],
            grants: serde_json::json!({ REPO: "read" }),
            repos,
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(r.red().contains("boss-reader/scratch"), "{}", r.text());
}

/// Every page is read: a grant on the third page is found.
#[test]
fn every_page_of_the_repository_listing_is_read() {
    let mut repos = vec![serde_json::json!({"full_name": "david/boss", "private": true})];
    repos.extend(
        (0..119)
            .map(|i| serde_json::json!({"full_name": format!("tenant/r{i:03}"), "private": true})),
    );
    let r = run(
        "paged",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid false",
            ],
            grants: serde_json::json!({ REPO: "read", "tenant/r115": "write" }),
            repos: serde_json::Value::Array(repos),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(r.red().contains("tenant/r115"), "{}", r.text());
}

#[test]
fn a_wider_grant_on_the_declared_repository_is_put_back_and_read_back() {
    let r = run(
        "wider",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid false",
            ],
            grants: serde_json::json!({ REPO: "write" }),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}", r.text());
    assert_eq!(r.puts().len(), 1, "{}", r.text());
    assert_eq!(r.grants, granted(), "{}", r.text());
    assert!(
        r.line().contains("was write"),
        "the drift is named: {}",
        r.line()
    );
}

#[test]
fn a_grant_that_answers_but_does_not_take_is_a_failure() {
    let r = run(
        "ignored",
        Fixture {
            faults: vec!["ignore_writes"],
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(r.line().contains("boss-reader: FAILED"), "{}", r.line());
}

#[test]
fn a_refused_grant_is_a_failure_naming_the_status() {
    let r = run(
        "refused",
        Fixture {
            put_status: Some("403"),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(r.line().contains("HTTP 403"), "{}", r.line());
}

#[test]
fn a_create_that_answers_but_leaves_no_account_is_a_failure() {
    let r = run(
        "create-ignored",
        Fixture {
            faults: vec!["create_ignored"],
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(r.puts().is_empty(), "{}", r.text());
    assert!(r.line().contains("boss-reader: FAILED"), "{}", r.line());
    r.nothing_secret_printed();
}

/// With no credential the account is still made and judged admin or not
/// — that half needs only the container — but no grant can be read, and
/// that is not a pass.
#[test]
fn no_credential_cannot_answer_for_the_grant() {
    let r = run(
        "no-auth",
        Fixture {
            no_auth: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}", r.text());
    assert_eq!(r.creates().len(), 1, "{}", r.text());
    assert!(
        r.calls().iter().all(|c| c.starts_with("docker ")),
        "no forge API call without a credential: {}",
        r.text()
    );
    assert!(r.line().contains("cannot answer"), "{}", r.line());
}

#[test]
fn an_unreachable_container_cannot_answer_and_writes_nothing() {
    let r = run(
        "no-docker",
        Fixture {
            faults: vec!["docker_fail"],
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}", r.text());
    assert!(r.creates().is_empty(), "{}", r.text());
    assert!(r.puts().is_empty(), "{}", r.text());
    assert!(r.line().contains("cannot answer"), "{}", r.line());
}

/// The control: a listing that does not show the declared repository is
/// a listing that cannot see private repositories, so the absence of any
/// other grant in it proves nothing.
#[test]
fn a_listing_blind_to_the_declared_repository_cannot_answer() {
    let r = run(
        "blind",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid false",
            ],
            grants: serde_json::json!({ REPO: "read" }),
            repos: serde_json::json!([{"full_name": "david/public-notes", "private": false}]),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}", r.text());
    assert!(r.line().contains("cannot answer"), "{}", r.line());
}

/// Seeing the declared repository does not prove the listing sees every
/// one (review eb947a26 of car 0a95d470, B1). A caller that administers
/// david/boss but cannot see another private repository lists david/boss
/// and nothing else, and until this repair that partial view was recorded
/// as `verified ... no other reachable` while a write grant on the hidden
/// repository stood. The forge's own inventory — the repository
/// directories inside its container, which do not depend on who asks — is
/// what the listing is now held to: a repository it holds that the
/// listing does not show is a cannot-answer, never a pass. Three legs over
/// one ledger: the partial view cannot answer; the complete view of the
/// same grants is RED, naming the hidden repository; the complete view
/// with no stray grant verifies (and a mixed-case name the forge stores
/// lowercased is matched, not counted as unseen).
#[test]
fn a_listing_that_shows_the_declared_repository_but_hides_another_cannot_answer() {
    let users = vec![
        "david david@example.invalid true",
        "boss-reader boss-reader@forge.invalid false",
    ];
    let repos = serde_json::json!([
        {"full_name": "david/boss", "private": true},
        {"full_name": "Tenant/Hidden", "private": true},
    ]);
    let stray = serde_json::json!({ REPO: "read", "Tenant/Hidden": "write" });

    let r = run(
        "partial-view",
        Fixture {
            users: users.clone(),
            repos: repos.clone(),
            listed: Some(serde_json::json!([{"full_name": "david/boss", "private": true}])),
            grants: stray.clone(),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}", r.text());
    let line = r.line();
    assert!(
        line.contains("cannot answer") && !line.contains("verified"),
        "a partial view is never a verification: {line}"
    );
    assert!(
        line.contains("1 repositories its listing does not show"),
        "the gap is counted: {line}"
    );
    assert!(
        !line.contains("Hidden") && !line.contains("hidden"),
        "a repository the credential cannot see is counted, not named: {line}"
    );
    assert_eq!(r.red(), "", "a cannot-answer files nothing: {}", r.text());
    assert_eq!(r.grants, stray, "no grant is removed on a guess");
    assert!(r.puts().is_empty(), "{}", r.text());
    r.nothing_secret_printed();

    let r = run(
        "complete-view-red",
        Fixture {
            users: users.clone(),
            repos: repos.clone(),
            grants: stray.clone(),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}", r.text());
    assert!(r.red().contains("Tenant/Hidden (write)"), "{}", r.text());
    assert_eq!(r.grants, stray, "the converge reds; it removes nothing");

    let r = run(
        "complete-view-verified",
        Fixture {
            users,
            repos,
            grants: granted(),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}", r.text());
    assert!(
        r.line()
            .contains("2 repositories read, every one of the 2 the forge holds"),
        "{}",
        r.line()
    );
}

/// The inventory is read before anything is claimed about it: refused, or
/// holding no declared repository (a wrong root answers with somebody
/// else's directories, or none), it is a cannot-answer.
#[test]
fn an_unreadable_or_foreign_inventory_cannot_answer() {
    let users = vec![
        "david david@example.invalid true",
        "boss-reader boss-reader@forge.invalid false",
    ];
    let r = run(
        "inventory-refused",
        Fixture {
            users: users.clone(),
            grants: granted(),
            faults: vec!["inventory_fail"],
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}", r.text());
    assert!(
        r.line()
            .contains("cannot answer — the forge's own repository inventory could not be read"),
        "{}",
        r.line()
    );

    let r = run(
        "inventory-foreign",
        Fixture {
            users,
            grants: granted(),
            disk: Some(serde_json::json!([{"full_name": "david/public-notes"}])),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}", r.text());
    assert!(
        r.line().contains("cannot answer") && r.line().contains("not david/boss"),
        "{}",
        r.line()
    );
}

#[test]
fn an_unreachable_forge_api_cannot_answer() {
    let r = run(
        "no-api",
        Fixture {
            users: vec![
                "david david@example.invalid true",
                "boss-reader boss-reader@forge.invalid false",
            ],
            faults: vec!["curl_fail"],
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}", r.text());
    assert!(r.puts().is_empty(), "no write on a guess: {}", r.text());
    assert!(r.line().contains("cannot answer"), "{}", r.line());
}

/// The declaration's own bounds: a name that is the site admin's, or a
/// permission wider than write, is refused before anything runs.
#[test]
fn the_declaration_declares_only_non_admin_collaborators() {
    let decl = declaration();
    let accounts = decl.as_array().expect("an array of accounts");
    assert!(!accounts.is_empty());
    for a in accounts {
        let name = a["name"].as_str().expect("a name");
        assert_ne!(name, "david", "the site admin is not a declared account");
        assert!(
            matches!(a["permission"].as_str(), Some("read" | "write")),
            "{name}: a collaborator is read or write, never admin: {a}"
        );
        assert!(
            a["email"].as_str().is_some_and(|e| e.ends_with(".invalid")),
            "{name}: an address that names nobody: {a}"
        );
        assert_eq!(a["repo"].as_str(), Some(REPO), "{name}: {a}");
    }
}

#[test]
fn a_declaration_naming_an_admin_permission_is_refused_before_any_call() {
    let dir = scratch_dir("forge-account-bad-decl");
    let decl = dir.join("accounts.json");
    write_file(
        &decl,
        r#"[{"name":"boss-reader","email":"boss-reader@forge.invalid","repo":"david/boss","permission":"admin"}]"#,
    );
    let log = dir.join("calls.log");
    write_file(&log, "");
    let out = Command::new("bash")
        .arg(script())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_FORGE_ACCOUNTS", &decl)
        .env("BOSS_FORGE_ACCOUNT_DOCKER", "/nonexistent/docker")
        .env("BOSS_FORGE_ACCOUNT_CURL", "/nonexistent/curl")
        .env("BOSS_FORGE_URL", "http://forge.test:3000")
        .output()
        .expect("bash runs");
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// --- the rules that mint for these accounts ----------------------------

/// `(rule name, forge_user, verify_repo, scopes)` for every rule handing
/// a forge user to a forgejo issuer handler.
fn forge_rules() -> Vec<(String, String, Option<String>, String)> {
    let lit = |v: &toml::Value| {
        v.as_str()
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let mut out = vec![];
    for entry in std::fs::read_dir(dispatcher_rules_dir()).expect("the rules directory") {
        let path = entry.expect("an entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a rule file");
        let doc: toml::Value =
            toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for rule in doc
            .get("rule")
            .and_then(|r| r.as_array())
            .into_iter()
            .flatten()
        {
            for step in rule
                .get("do")
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
            {
                let handler = step["handler"].as_str().unwrap_or_default();
                if !handler.starts_with("credential.rotate.forgejo") {
                    continue;
                }
                let args = &step["args"];
                let Some(user) = args.get("forge_user") else {
                    continue;
                };
                out.push((
                    rule["name"].as_str().unwrap_or_default().to_string(),
                    lit(user),
                    args.get("verify_repo").map(lit),
                    args.get("scopes").map(lit).unwrap_or_default(),
                ));
            }
        }
    }
    out
}

/// §9a: an account lives in the declaration (the converge makes it) and in
/// each rule that mints for it (the broker). A rule naming an account the
/// converge never makes mints for nobody; one whose proof reads another
/// repository proves the wrong grant.
#[test]
fn every_forge_user_a_broker_rule_names_is_david_or_a_declared_account() {
    let decl = declaration();
    let accounts = decl.as_array().expect("an array");
    let rules = forge_rules();
    assert!(
        rules.len() >= 7,
        "control: the forge rules are found: {rules:?}"
    );
    for (name, user, verify_repo, scopes) in &rules {
        if user == "david" {
            continue;
        }
        let a = accounts
            .iter()
            .find(|a| a["name"].as_str() == Some(user.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "{name} mints for forge user {user}, which infra/forge/forge-accounts.json \
                     does not declare — the forge converge never makes it"
                )
            });
        assert_eq!(
            verify_repo.as_deref(),
            a["repo"].as_str(),
            "{name}: the proof reads the account's one repository"
        );
        if scopes.contains("write:") {
            assert_eq!(
                a["permission"], "write",
                "{name} mints a write token for {user}, a read collaborator"
            );
        }
    }
}

/// The decision itself (design 0af333ee, question `accounts`): the
/// root-tree token is boss-reader's, and only the root-tree token moves in
/// this car — the writers move one rotation at a time, later.
#[test]
fn the_root_tree_token_is_minted_for_boss_reader() {
    let rules = forge_rules();
    let users: Vec<_> = rules
        .iter()
        .filter(|(n, ..)| n.contains("boss-gcp-root-tree-token"))
        .map(|(n, u, ..)| (n.as_str(), u.as_str()))
        .collect();
    assert_eq!(
        users.len(),
        2,
        "the rotation and its delivery twin: {users:?}"
    );
    for (n, u) in users {
        assert_eq!(u, ACCOUNT, "{n}");
    }
}
