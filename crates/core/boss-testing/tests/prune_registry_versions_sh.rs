//! `infra/forge/prune-registry-versions.sh` is RUN, not read — against a
//! stubbed `curl` (the Forgejo packages API, the `/v2` manifest reads,
//! the system of record's train listing) and a stubbed `kubectl` (the
//! cluster's images), both recording every call — so every verdict
//! below is one the script actually reached. Nothing here touches the
//! forge, the registry, or a cluster.
//!
//! WHY THE VERB EXISTS (backlog 9789a827, David 2026-09-17). Measured
//! 2026-09-16: /opt/forgejo/data is 93 GB of the forge's 228 GB disk —
//! the container registry every train pushes a `boss:<sha>` image to
//! (~1–3 GB each, plus `boss-ci:<sha>` per train) — and nothing prunes
//! the REGISTRY side: prune-registry-tags.lib.sh removes LOCAL docker
//! tags only after the registry holds them (the registry IS the rollback
//! path), and disk-floor-sweep stops at the floor. The replacement disk
//! was not delivered, so the registry grows ~2 GB per train. The verb
//! deletes registry versions older than the last N landed trains,
//! keeping the live image, the rollback target, `latest`, anything under
//! 24 h, and everything it cannot classify.
//!
//! What each case pins:
//!
//!   * THE CREDENTIAL NEVER APPEARS IN ANY OUTPUT — the token, its
//!     base64 `auth` form, and the bearer minted from it — on any path.
//!     It rides in a curl config file, never in argv, and the stub
//!     verifies the header is exactly the docker login's.
//!   * THE KEEP SET IS DERIVED, AND A HALF THAT CANNOT BE DERIVED IS A
//!     REFUSAL: an unreadable cluster, a missing stamp, no landed shas,
//!     a docker config with no auth for the registry — each refuses
//!     naming the half, and nothing is deleted.
//!   * A 403 NAMES THE SCOPE the forge asked for — `read:package` on
//!     the listing, `write:package` on the first DELETE — and the real
//!     run stops there, before a second attempt.
//!   * THE DRY RUN SENDS NO DELETE and prints the same record line the
//!     real run would, flagged `dry_run: true`.
//!   * THE REAL RUN DELETES EXACTLY THE PLANNED SET — tags first, then
//!     the untagged child manifests only those tags referenced, then
//!     orphans no readable index references — and never a child a kept
//!     tag shares.
//!   * A TAG WHOSE INDEX CANNOT BE READ IS NOT DELETED, and while any
//!     index is unreadable no orphan is either: unclassified is kept.
//!   * THE VERB FILE serves the forge, is MUTATING, names David and the
//!     packet, takes mode plus an optional keep count, and declares a
//!     timeout for ~1,400 versions of reads.
//!   * THE VERDICT COMES FIRST (backlog 5323f3ef, measured 2026-09-17
//!     05:27Z on the first dry run, ops-request 279b8659): the verb
//!     printed 1,313 per-version lines (165 KB) and the JSON record
//!     LAST; the ops runner keeps the first 100 KB, so the packet held
//!     the plan's head and none of its verdict. Now the record and the
//!     summary precede the per-version list in the runner's combined
//!     capture, and the full list is a file on the forge the record
//!     names — refused before any DELETE when it cannot be written.
//!   * A REAL RUN'S LINE CARRIES ITS OUTCOME (backlog ea67ad87, measured
//!     on the first real prune, ops-request 973beaa2, 2026-09-17
//!     13:41Z): the packet read 898 `would DELETE` lines under a verdict
//!     that said OK. Now DELETED / GONE / FAILED <code> / KEPT, and only
//!     a dry run says `would DELETE`; the file on disk is unchanged.
//!   * THE CLEANUP CADENCE IS READ, NOT TYPED: df moved 724 KB on 1,337
//!     deletions because Forgejo's `[cron.cleanup_packages]` frees the
//!     blobs, and its app.ini is not in this tree — the record carries
//!     what the verb read off the forge's app.ini, or that it could not.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/forge/prune-registry-versions.sh";
const TOKEN: &str = "forgetok0123456789abcdefFORGE";
/// base64("david:forgetok0123456789abcdefFORGE") — what `docker login`
/// writes under `auths."10.20.0.15:3000".auth`.
const AUTH_B64: &str = "ZGF2aWQ6Zm9yZ2V0b2swMTIzNDU2Nzg5YWJjZGVmRk9SR0U=";
const BEARER: &str = "stub-bearer-jwt-9f8e7d6c";
const REGISTRY: &str = "10.20.0.15:3000/david/boss";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// An RFC 3339 stamp `hours` ago, the way GNU date prints it — the
/// script parses `created_at` with the same tool.
fn hours_ago(hours: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let at = now - hours * 3600;
    let out = Command::new("date")
        .args(["-u", "-d", &format!("@{at}"), "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .expect("date runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn version(id: u64, name: &str, version: &str, created_at: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "owner": {"login": "david"},
        "type": "container",
        "name": name,
        "version": version,
        "created_at": created_at,
    })
}

/// A manifest index (what `docker push` puts under a tag when buildx
/// attaches provenance): the children are untagged `sha256:` versions
/// in the registry, and they are where the layer files live.
fn index(children: &[&str]) -> String {
    serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": children.iter().map(|c| serde_json::json!({
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{c}"),
            "size": 1234,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

fn digest(name: &str) -> String {
    // 64 hex characters, distinct per name, so the fixture reads like
    // the registry does.
    let mut s = String::new();
    for b in name.bytes().cycle().take(32) {
        s.push_str(&format!("{b:02x}"));
    }
    format!("sha256:{s}")
}

/// The registry as the fixture declares it, all on the versions the
/// keep set must judge. Ages: fresh = 2 h, old = 5 days.
///
///   boss:
///     bdf435d  live (deploy boss in ns boss)          keep: live
///     aaaaaaa  live (deploy boss in ns boss-playground) keep: live
///     b2814ef  the stamp                              keep: stamp
///     1111111  landed train                           keep: landed
///     2222222  landed train                           keep: landed
///     f0e5401  fresh (2 h)                            keep: fresh
///     latest                                          keep: latest
///     v1       not a sha                              unclassified
///     01d0001  old, no keep reason                    DELETE (tag)
///     deadbee  old, no keep reason                    DELETE (tag)
///     children: live1, live2 (of bdf435d); shared (of bdf435d AND
///     deadbee → kept); old1, old2 (of 01d0001 → DELETE); dead1 (of
///     deadbee → DELETE); stamp1 (of b2814ef → kept); orphan-old (no
///     index → DELETE); orphan-fresh (no index, 2 h → kept)
///   boss-ci (page 2):
///     1111111…(40)  landed → keep;  01d0001…(40) old → DELETE with
///     child ci-old1;  rust1.96 → unclassified, child ci-base kept
///   boss-ci-cache: never a candidate (not one of the two names)
struct Case {
    bin: PathBuf,
    stub: PathBuf,
    curl_log: PathBuf,
    deletes: PathBuf,
    kubectl_log: PathBuf,
    docker_config: PathBuf,
    stamp: PathBuf,
    df_path: PathBuf,
    list_dir: PathBuf,
}

fn forty(seven: &str) -> String {
    let mut s = seven.to_string();
    while s.len() < 40 {
        s.push_str(seven);
    }
    s.truncate(40);
    s
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("prune-registry-versions-{name}"));
        let bin = root.join("bin");
        let stub = root.join("stub");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&stub).unwrap();
        let curl_log = root.join("curl.log");
        let deletes = root.join("deletes.log");
        let kubectl_log = root.join("kubectl.log");
        let docker_config = root.join("docker-config.json");
        let stamp = root.join("boss-last-built");
        let df_path = root.join("forgejo-data");
        std::fs::create_dir_all(&df_path).unwrap();
        // Where the per-version list lands (the forge's
        // /var/backups/boss/registry-prune in production): not created
        // here — the script creates it.
        let list_dir = root.join("registry-prune");

        write_file(
            &docker_config,
            &format!(
                r#"{{"auths": {{"10.20.0.15:3000": {{"auth": "{AUTH_B64}"}}, "https://index.docker.io/v1/": {{"auth": "bm9ib2R5OmJvZ3Vz"}}}}}}"#
            ),
        );
        write_file(&stamp, "b2814ef\n");

        let fresh = hours_ago(2);
        let old = hours_ago(5 * 24);
        // One old stamp written the way Forgejo prints it (an offset,
        // not Z), so the parse is exercised on both shapes.
        let old_offset = "2026-09-01T00:00:00-07:00";
        let mut id = 3000u64;
        let mut next = || {
            id += 1;
            id
        };
        let page1 = serde_json::json!([
            version(next(), "boss", "bdf435d", &fresh),
            version(next(), "boss", &digest("live1"), &fresh),
            version(next(), "boss", &digest("live2"), &fresh),
            version(next(), "boss", &digest("shared"), &old),
            version(next(), "boss", "aaaaaaa", &old),
            version(next(), "boss", "b2814ef", &old),
            version(next(), "boss", &digest("stamp1"), &old),
            version(next(), "boss", "1111111", &old),
            version(next(), "boss", "2222222", old_offset),
            version(next(), "boss", "f0e5401", &fresh),
            version(next(), "boss", "latest", &old),
            version(next(), "boss", "v1", &old),
            version(next(), "boss", "01d0001", &old),
            version(next(), "boss", &digest("old1"), &old),
            version(next(), "boss", &digest("old2"), &old),
            version(next(), "boss", "deadbee", old_offset),
            version(next(), "boss", &digest("dead1"), &old),
            version(next(), "boss", &digest("orphan-old"), &old),
            version(next(), "boss", &digest("orphan-fresh"), &fresh),
            version(next(), "boss-ci-cache", "deadbee", &old),
        ]);
        let page2 = serde_json::json!([
            version(next(), "boss-ci", &forty("1111111"), &old),
            version(next(), "boss-ci", &forty("01d0001"), &old),
            version(next(), "boss-ci", &digest("ci-old1"), &old),
            version(next(), "boss-ci", "rust1.96", &old),
            version(next(), "boss-ci", &digest("ci-base"), &old),
        ]);
        write_file(&stub.join("page-1.json"), &page1.to_string());
        write_file(&stub.join("page-2.json"), &page2.to_string());
        write_file(&stub.join("empty.json"), "[]");
        write_file(&stub.join("total"), "25");
        write_file(
            &stub.join("token.json"),
            &format!(r#"{{"token": "{BEARER}"}}"#),
        );
        write_file(
            &stub.join("notfound.json"),
            r#"{"errors":[{"code":"MANIFEST_UNKNOWN","message":"manifest unknown"}]}"#,
        );
        write_file(
            &stub.join("forbidden-read.json"),
            r#"{"message":"token does not have at least one of required scope(s), required=[read:package]","url":"http://10.20.0.15:3000/api/swagger"}"#,
        );
        write_file(
            &stub.join("forbidden-write.json"),
            r#"{"message":"token does not have at least one of required scope(s), required=[write:package]","url":"http://10.20.0.15:3000/api/swagger"}"#,
        );
        let idx = |tag: &str, kids: &[&str]| {
            write_file(&stub.join(format!("index-boss-{tag}.json")), &index(kids));
        };
        let d = |n: &str| digest(n).trim_start_matches("sha256:").to_string();
        idx("bdf435d", &[&d("live1"), &d("live2"), &d("shared")]);
        idx("aaaaaaa", &[]);
        idx("b2814ef", &[&d("stamp1")]);
        idx("1111111", &[]);
        idx("2222222", &[]);
        idx("f0e5401", &[]);
        idx("latest", &[]);
        idx("v1", &[]);
        idx("01d0001", &[&d("old1"), &d("old2")]);
        idx("deadbee", &[&d("dead1"), &d("shared")]);
        write_file(
            &stub.join(format!("index-boss-ci-{}.json", forty("1111111"))),
            &index(&[]),
        );
        write_file(
            &stub.join(format!("index-boss-ci-{}.json", forty("01d0001"))),
            &index(&[&d("ci-old1")]),
        );
        write_file(
            &stub.join("index-boss-ci-rust1.96.json"),
            &index(&[&d("ci-base")]),
        );

        // The system of record: two closed trains (1111111 landed via
        // train_ref, 2222222 via merge_ref) and one open train that
        // references 3333333.
        write_file(
            &stub.join("trains.json"),
            &serde_json::json!({
                "total": 3,
                "data": [
                    {"status": "open", "steps": [{"metadata": {"train_ref": "train/2026-09-17-0100@3333333"}}]},
                    {"status": "closed", "steps": [{"metadata": {"train_ref": "train/2026-09-17-0000@1111111"}}, {"metadata": {"merge_ref": "222222222222"}}]},
                    {"status": "closed", "steps": [{"metadata": {"train_ref": "train/2026-09-16-2300@1111111"}}]},
                ]
            })
            .to_string(),
        );

        // The cluster's images, as `kubectl get deploy,sts,cronjob -A
        // -o json` lists them: the prod deploy on bdf435d (main and
        // init), a playground instance on aaaaaaa, chores on latest,
        // the dev pod on boss-ci:rust1.96.
        write_file(
            &stub.join("cluster.json"),
            &serde_json::json!({
                "kind": "List",
                "items": [
                    {"kind": "Deployment", "metadata": {"name": "boss", "namespace": "boss"},
                     "spec": {"template": {"spec": {
                        "initContainers": [{"name": "boss-init", "image": format!("{REGISTRY}:bdf435d")}],
                        "containers": [{"name": "boss", "image": format!("{REGISTRY}:bdf435d")}]}}}},
                    {"kind": "Deployment", "metadata": {"name": "boss", "namespace": "boss-playground"},
                     "spec": {"template": {"spec": {
                        "containers": [{"name": "boss", "image": format!("{REGISTRY}:aaaaaaa")}]}}}},
                    {"kind": "StatefulSet", "metadata": {"name": "postgres", "namespace": "boss"},
                     "spec": {"template": {"spec": {
                        "containers": [{"name": "postgres", "image": "10.20.0.15:3000/david/postgres:16"}]}}}},
                    {"kind": "CronJob", "metadata": {"name": "boss-files-gc", "namespace": "boss"},
                     "spec": {"jobTemplate": {"spec": {"template": {"spec": {
                        "containers": [{"name": "chore", "image": format!("{REGISTRY}:latest")}]}}}}}},
                    {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev"},
                     "spec": {"template": {"spec": {
                        "containers": [{"name": "dev", "image": "10.20.0.15:3000/david/boss-ci:rust1.96"}]}}}},
                ]
            })
            .to_string(),
        );

        // The stub curl. Every call is logged as `METHOD URL auth=<kind>`
        // where <kind> says which credential the -K config carried:
        // `basic-ok` (the docker login's exact header), `bearer-ok` (the
        // minted token), `none`, or `other`. Answers come from $STUB_DIR
        // by URL shape; a DELETE is appended to $STUB_DELETES and answers
        // 204 unless `delete-code` (with `delete-body`) or
        // `delete-fail-on` (a version: 500) or `delete-gone-on` (a
        // version: 404, deleted by someone else first) says otherwise. A
        // version a DELETE removed (204, or the someone-else 404) leaves
        // every later listing, so the re-list reads the effect; a
        // `delete-code` answer removes nothing, and `delete-survives` (a
        // version) answers 204 and stays listed. `-f` fails on a
        // 4xx with exit 22, the way curl does (landed_train_shas uses
        // -fsS).
        write_exec(
            &bin.join("curl"),
            r#"#!/usr/bin/env bash
set -u
method=GET; out=""; want_code=0; url=""; cfg=""; hdr=""; fail=0
args=("$@"); i=0
while [ $i -lt ${#args[@]} ]; do
    a="${args[$i]}"
    case "$a" in
        -o) i=$((i+1)); out="${args[$i]}" ;;
        -w) i=$((i+1)); want_code=1 ;;
        -X) i=$((i+1)); method="${args[$i]}" ;;
        -H) i=$((i+1)) ;;
        -K) i=$((i+1)); cfg="${args[$i]}" ;;
        -D) i=$((i+1)); hdr="${args[$i]}" ;;
        --max-time) i=$((i+1)) ;;
        --*) ;;
        -*) case "$a" in *f*) fail=1 ;; esac ;;
        *) url="$a" ;;
    esac
    i=$((i+1))
done
auth=none
if [ -n "$cfg" ] && [ -f "$cfg" ]; then
    if grep -qxF "header = \"Authorization: Basic $STUB_AUTH_B64\"" "$cfg"; then auth=basic-ok
    elif grep -qxF "header = \"Authorization: Bearer $STUB_BEARER\"" "$cfg"; then auth=bearer-ok
    elif grep -q 'Authorization' "$cfg"; then auth=other
    fi
fi
printf '%s %s auth=%s\n' "$method" "$url" "$auth" >> "$STUB_CURL_LOG"
emit() { # <code> <body-file>
    local code="$1" body="$2"
    if [ -n "$hdr" ]; then
        # The forge's count is the registry NOW: what a DELETE removed
        # is no longer counted. `total-text` sends a header that is not
        # a number at all.
        tot=$(cat "$STUB_DIR/total")
        if [ -f "$STUB_DIR/total-text" ]; then tot=$(cat "$STUB_DIR/total-text")
        else rmn=$(grep -c . "$STUB_DIR/removed" 2>/dev/null || true); tot=$((tot - ${rmn:-0})); fi
        printf 'HTTP/1.1 %s\r\nX-Total-Count: %s\r\n\r\n' "$code" "$tot" > "$hdr"
    fi
    if [ -n "$out" ]; then cat "$body" > "$out"; else cat "$body"; fi
    [ "$want_code" = 1 ] && printf '%s' "$code"
    if [ "$fail" = 1 ] && [ "$code" -ge 400 ]; then exit 22; fi
    exit 0
}
case "$url" in
    */api/jobs?kind=pr-train*)
        [ -f "$STUB_DIR/trains-down" ] && { echo 'curl: (7) Failed to connect' >&2; exit 7; }
        emit 200 "$STUB_DIR/trains.json" ;;
    */api/v1/packages/david?type=container*)
        [ "$auth" = basic-ok ] || emit 401 "$STUB_DIR/notfound.json"
        if [ -f "$STUB_DIR/list-code" ]; then emit "$(cat "$STUB_DIR/list-code")" "$STUB_DIR/list-body"; fi
        page=$(printf '%s' "$url" | sed -n 's/.*[?&]page=\([0-9]*\).*/\1/p')
        f="$STUB_DIR/page-$page.json"; [ -f "$f" ] || f="$STUB_DIR/empty.json"
        # After the first DELETE, the re-list can be made to lie:
        # `relist-empty` answers [] for every page, `relist-short` [] from
        # page 2 on, `relist-body` replaces page 1.
        if [ -s "$STUB_DELETES" ]; then
            [ -f "$STUB_DIR/relist-empty" ] && emit 200 "$STUB_DIR/empty.json"
            if [ -f "$STUB_DIR/relist-short" ] && [ "$page" -ge 2 ]; then emit 200 "$STUB_DIR/empty.json"; fi
            if [ -f "$STUB_DIR/relist-body" ] && [ "$page" = 1 ]; then emit 200 "$STUB_DIR/relist-body"; fi
            # `relist-hide` (`name version`): a version nobody deleted
            # vanishes from the re-list AND from its count, so the count
            # agrees and only the positive control can see it.
            if [ -f "$STUB_DIR/relist-hide" ] && ! grep -qxF "$(cat "$STUB_DIR/relist-hide")" "$STUB_DIR/removed" 2>/dev/null; then
                cat "$STUB_DIR/relist-hide" >> "$STUB_DIR/removed"
            fi
        fi
        # The registry as it stands NOW: a version a DELETE really
        # removed is gone from every later listing (the re-list).
        if [ -s "$STUB_DIR/removed" ]; then
            jq -c --rawfile r "$STUB_DIR/removed" '($r | split("\n")) as $rm | map(select(((.name + " " + .version) as $k | $rm | index($k)) | not))' "$f" > "$STUB_DIR/page-now.json"
            f="$STUB_DIR/page-now.json"
        fi
        emit 200 "$f" ;;
    */v2/token*)
        [ "$auth" = basic-ok ] || emit 401 "$STUB_DIR/notfound.json"
        [ -f "$STUB_DIR/no-token" ] && emit 401 "$STUB_DIR/notfound.json"
        emit 200 "$STUB_DIR/token.json" ;;
    */v2/david/*/manifests/*)
        [ "$auth" = bearer-ok ] || emit 401 "$STUB_DIR/notfound.json"
        rest="${url#*/v2/david/}"; name="${rest%%/manifests/*}"; tag="${rest##*/manifests/}"
        f="$STUB_DIR/index-$name-$tag.json"
        [ -f "$f" ] && emit 200 "$f"
        emit 404 "$STUB_DIR/notfound.json" ;;
    */api/v1/packages/david/container/*)
        [ "$method" = DELETE ] || { echo "stub curl: unexpected $method on $url" >&2; exit 1; }
        [ "$auth" = basic-ok ] || emit 401 "$STUB_DIR/notfound.json"
        rest="${url#*/container/}"; name="${rest%%/*}"; ver="${rest#*/}"
        printf '%s %s\n' "$name" "$ver" >> "$STUB_DELETES"
        # How many outcomes the list file already held when this DELETE
        # was sent: one number per DELETE, so a test can see the file
        # grow as the deletes happen rather than after the last one.
        cat "$BOSS_PRUNE_LIST_DIR"/*.txt 2>/dev/null | grep -c '^deleted ' >> "$STUB_DIR/listed-before-delete"
        if [ -f "$STUB_DIR/delete-code" ]; then emit "$(cat "$STUB_DIR/delete-code")" "$STUB_DIR/delete-body"; fi
        if [ -f "$STUB_DIR/delete-fail-on" ] && [ "$ver" = "$(cat "$STUB_DIR/delete-fail-on")" ]; then emit 500 "$STUB_DIR/notfound.json"; fi
        if [ -f "$STUB_DIR/delete-gone-on" ] && [ "$ver" = "$(cat "$STUB_DIR/delete-gone-on")" ]; then
            printf '%s %s\n' "$name" "$ver" >> "$STUB_DIR/removed"
            emit 404 "$STUB_DIR/notfound.json"
        fi
        # `delete-survives`: a 204 whose version stays listed — the
        # answer without the effect.
        if ! { [ -f "$STUB_DIR/delete-survives" ] && [ "$ver" = "$(cat "$STUB_DIR/delete-survives")" ]; }; then
            printf '%s %s\n' "$name" "$ver" >> "$STUB_DIR/removed"
        fi
        emit 204 /dev/null ;;
esac
echo "stub curl: unexpected url $url" >&2
exit 1
"#,
        );
        // The stub kubectl: logs argv; answers the one read the script
        // makes with the cluster fixture, or fails on STUB_KUBECTL_FAIL.
        write_exec(
            &bin.join("kubectl"),
            r#"#!/usr/bin/env bash
set -u
{ printf '%s\n' "$@"; echo '=== call ==='; } >> "$STUB_KUBECTL_LOG"
[ -n "${STUB_KUBECTL_FAIL:-}" ] && { echo 'error: You must be logged in to the server (Unauthorized)' >&2; exit 1; }
case " $* " in
    *" get deploy,sts,cronjob -A -o json "*) cat "$STUB_DIR/cluster.json"; exit 0 ;;
esac
echo "stub kubectl: unexpected argv: $*" >&2
exit 1
"#,
        );
        Case {
            bin,
            stub,
            curl_log,
            deletes,
            kubectl_log,
            docker_config,
            stamp,
            df_path,
            list_dir,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT)).args(args);
        self.env(&mut cmd, extra);
        let out = cmd.output().expect("prune-registry-versions.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// The streams interleaved the way the ops runner captures them
    /// (`> raw 2>&1`, ops-runner.sh) — the only reading in which the
    /// ORDER of the record against the per-version lines means
    /// anything, since the runner keeps the first 100 KB of that file.
    fn run_combined(&self, args: &[&str]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(r#"exec bash "$@" 2>&1"#)
            .arg("_")
            .arg(repo_root().join(SCRIPT))
            .args(args);
        self.env(&mut cmd, &[]);
        let out = cmd.output().expect("prune-registry-versions.sh runs");
        assert!(out.stderr.is_empty(), "stderr was not merged");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn env(&self, cmd: &mut Command, extra: &[(&str, String)]) {
        cmd.env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_KUBECTL", self.bin.join("kubectl"))
            .env("BOSS_JOBS_URL", "http://sor.invalid:7900")
            .env("BOSS_FORGE_REGISTRY", REGISTRY)
            .env("BOSS_PRUNE_DOCKER_CONFIG", &self.docker_config)
            .env("BOSS_FORGE_LAST_BUILT", &self.stamp)
            .env("BOSS_PRUNE_DF_PATH", &self.df_path)
            .env("BOSS_PRUNE_LIST_DIR", &self.list_dir)
            .env("STUB_DIR", &self.stub)
            .env("STUB_CURL_LOG", &self.curl_log)
            .env("STUB_DELETES", &self.deletes)
            .env("STUB_KUBECTL_LOG", &self.kubectl_log)
            .env("STUB_AUTH_B64", AUTH_B64)
            .env("STUB_BEARER", BEARER);
        for (k, v) in extra {
            cmd.env(k, v);
        }
    }

    /// The DELETEs the registry saw, in order, as `name version`.
    fn deleted(&self) -> Vec<String> {
        std::fs::read_to_string(&self.deletes)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn curl_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.curl_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The one JSON record line on stdout — the line that names the
    /// verb (a failure also echoes the forge's JSON body, on stderr).
    /// Found by content, not position; in the runner's combined capture
    /// it is the FIRST JSON line, and
    /// `the_verdict_precedes_the_per_version_list_and_the_list_is_a_named_file`
    /// pins that.
    fn record(&self, text: &str) -> serde_json::Value {
        text.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["verb"] == "prune-registry-versions")
            .unwrap_or_else(|| panic!("no JSON record line in:\n{text}"))
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

fn no_credential(text: &str, what: &str) {
    for secret in [TOKEN, AUTH_B64, BEARER, "forgetok"] {
        assert!(
            !text.contains(secret),
            "{what}: the credential ({secret}) reached the output:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The mode and the arguments.
// ---------------------------------------------------------------------------

#[test]
fn refuses_a_missing_or_foreign_mode_or_keep_count() {
    if !has("jq") {
        return;
    }
    let c = Case::new("args");
    for (args, why) in [
        (vec![], "no mode"),
        (vec!["--yes"], "a foreign mode"),
        (vec!["--dry-run", "0"], "a zero keep count"),
        (vec!["--dry-run", "ten"], "a non-numeric keep count"),
        (vec!["--dry-run", "10", "extra"], "a non-numeric ceiling"),
        (vec!["--dry-run", "10", "8", "extra"], "a fourth word"),
    ] {
        let (rc, out) = c.run(&args);
        assert_eq!(rc, 2, "{why}: {out}");
        assert!(c.deleted().is_empty(), "{why}: something was deleted");
        no_credential(&out, why);
    }
    let (rc, out) = c.run(&["--dry-run", "5"]);
    assert_eq!(rc, 0, "a keep count of 5 is a valid argument: {out}");
}

#[test]
fn refuses_without_a_system_of_record_or_a_registry_credential() {
    if !has("jq") {
        return;
    }
    let c = Case::new("no-sor");
    let (rc, out) = c.run_env(&["--dry-run"], &[("BOSS_JOBS_URL", String::new())]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["BOSS_JOBS_URL", "Nothing was deleted"], "no SoR");

    // No auth for the registry host in the docker config: the refusal
    // names the file and the registry key, never a value.
    let c = Case::new("no-auth");
    write_file(
        &c.docker_config,
        r#"{"auths": {"https://index.docker.io/v1/": {"auth": "bm9ib2R5OmJvZ3Vz"}}}"#,
    );
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &[
            "10.20.0.15:3000",
            "docker-config.json",
            "docker login",
            "Nothing was deleted",
        ],
        "no auth for the registry",
    );
    assert!(
        !out.contains("bm9ib2R5"),
        "another registry's auth leaked: {out}"
    );

    // A credential store instead of an inline auth: refused, named.
    let c = Case::new("credstore");
    write_file(
        &c.docker_config,
        r#"{"auths": {"10.20.0.15:3000": {}}, "credsStore": "pass"}"#,
    );
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["credsStore", "Nothing was deleted"], "credsStore");

    // An absent file.
    let c = Case::new("no-config");
    std::fs::remove_file(&c.docker_config).unwrap();
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["docker-config.json", "Nothing was deleted"],
        "no config",
    );
    assert!(c.curl_calls().is_empty(), "no credential, no call: {out}");
}

// ---------------------------------------------------------------------------
// The keep set: every half that cannot be derived is a refusal.
// ---------------------------------------------------------------------------

#[test]
fn refuses_when_the_cluster_the_stamp_or_the_landed_trains_cannot_be_read() {
    if !has("jq") {
        return;
    }
    let c = Case::new("no-cluster");
    let (rc, out) = c.run_env(&["--dry-run"], &[("STUB_KUBECTL_FAIL", "1".into())]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["live", "kubectl", "Nothing was deleted"],
        "kubectl fails",
    );
    assert!(c.deleted().is_empty());

    // A cluster with no deploy boss in ns boss: the named live image
    // is not there, so the keep set is not derivable.
    let c = Case::new("no-live");
    write_file(
        &c.stub.join("cluster.json"),
        r#"{"kind":"List","items":[]}"#,
    );
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["deploy", "boss", "Nothing was deleted"],
        "no live image",
    );

    let c = Case::new("no-stamp");
    std::fs::remove_file(&c.stamp).unwrap();
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["boss-last-built", "rollback", "Nothing was deleted"],
        "no stamp",
    );

    let c = Case::new("bad-stamp");
    write_file(&c.stamp, "not a sha\n");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["boss-last-built", "Nothing was deleted"],
        "bad stamp",
    );

    // No landed trains: the SoR listed none (a failed read, not a fact).
    let c = Case::new("no-landed");
    write_file(&c.stub.join("trains.json"), r#"{"total": 0, "data": []}"#);
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["landed", "Nothing was deleted"], "no landed trains");

    let c = Case::new("sor-down");
    write_file(&c.stub.join("trains-down"), "");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["landed", "Nothing was deleted"], "SoR down");
    // The keep set is derived BEFORE the registry is read: no listing,
    // no manifest read, and nothing deleted.
    assert!(
        !c.curl_calls()
            .iter()
            .any(|l| l.contains("/api/v1/packages/")),
        "the registry was read before the keep set existed: {:?}",
        c.curl_calls()
    );
}

#[test]
fn a_403_on_the_listing_names_the_scope_and_deletes_nothing() {
    if !has("jq") {
        return;
    }
    let c = Case::new("forbidden-read");
    write_file(&c.stub.join("list-code"), "403");
    std::fs::copy(c.stub.join("forbidden-read.json"), c.stub.join("list-body")).unwrap();
    for mode in ["--dry-run", "--for-real"] {
        let (rc, out) = c.run(&[mode]);
        assert_eq!(rc, 2, "{mode}: {out}");
        contains_all(
            &out,
            &["read:package", "403", "David", "Nothing was deleted"],
            mode,
        );
        no_credential(&out, mode);
        assert!(c.deleted().is_empty(), "{mode} deleted something");
    }
}

// ---------------------------------------------------------------------------
// The dry run.
// ---------------------------------------------------------------------------

#[test]
fn dry_run_plans_the_set_sends_no_delete_and_records() {
    if !has("jq") {
        return;
    }
    let c = Case::new("dry-run");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "{out}");
    no_credential(&out, "dry run");
    assert!(
        c.deleted().is_empty(),
        "the dry run deleted: {:?}",
        c.deleted()
    );
    assert!(
        !c.curl_calls().iter().any(|l| l.starts_with("DELETE ")),
        "the dry run sent a DELETE: {:?}",
        c.curl_calls()
    );
    // The keep set is stated, half by half.
    contains_all(
        &out,
        &[
            "live: bdf435d",
            "aaaaaaa",
            "stamp: b2814ef",
            "landed: 2",
            "latest",
            "24",
        ],
        "keep set",
    );
    // The plan names every deletion and every unclassified version.
    contains_all(
        &out,
        &[
            "would DELETE boss:01d0001",
            "would DELETE boss:deadbee",
            &format!("would DELETE boss:{}", digest("old1")),
            &format!("would DELETE boss:{}", digest("dead1")),
            &format!("would DELETE boss:{}", digest("orphan-old")),
            &format!("would DELETE boss-ci:{}", forty("01d0001")),
            &format!("would DELETE boss-ci:{}", digest("ci-old1")),
            "unclassified boss:v1",
            "unclassified boss-ci:rust1.96",
            "DRY RUN",
            "Nothing was deleted",
        ],
        "plan",
    );
    for kept in [
        "boss:bdf435d",
        "boss:b2814ef",
        "boss:1111111",
        "boss:2222222",
        "boss:f0e5401",
        "boss:latest",
        &format!("boss:{}", digest("shared")),
        &format!("boss:{}", digest("stamp1")),
        &format!("boss:{}", digest("orphan-fresh")),
        &format!("boss-ci:{}", forty("1111111")),
        &format!("boss-ci:{}", digest("ci-base")),
    ] {
        assert!(
            !out.contains(&format!("would DELETE {kept}")),
            "{kept} is in the keep set and was planned for deletion:\n{out}"
        );
    }
    let r = c.record(&out);
    assert_eq!(r["verb"], "prune-registry-versions");
    assert_eq!(r["dry_run"], true);
    assert_eq!(r["keep"]["stamp"], "b2814ef");
    assert_eq!(r["keep"]["landed_trains"], 2);
    assert_eq!(r["keep"]["keep_trains"], 10);
    assert_eq!(r["keep"]["newer_than_hours"], 24);
    let live = r["keep"]["live"].as_array().unwrap();
    assert!(live.iter().any(|v| v == "boss:bdf435d"), "{live:?}");
    assert!(live.iter().any(|v| v == "boss:aaaaaaa"), "{live:?}");
    let boss = &r["packages"]["boss"];
    assert_eq!(boss["versions"], 19);
    assert_eq!(boss["delete"], 6, "{boss}");
    assert_eq!(boss["unclassified"], 1, "{boss}");
    assert_eq!(boss["kept"], 12, "{boss}");
    assert_eq!(boss["deleted"], 0);
    let ci = &r["packages"]["boss-ci"];
    assert_eq!(ci["versions"], 5);
    assert_eq!(ci["delete"], 2, "{ci}");
    assert_eq!(ci["unclassified"], 1, "{ci}");
    assert!(r["packages"].get("boss-ci-cache").is_none(), "{r}");
    assert_eq!(r["listed"], 25);
    assert_eq!(r["declared_bytes"], serde_json::Value::Null);
    assert!(r["disk_avail_kb_before"].is_number(), "{r}");
    // The scope the real run needs is stated as unmeasured — a dry run
    // cannot prove write:package without deleting.
    assert!(
        r["write_scope"].as_str().unwrap().contains("unmeasured"),
        "{r}"
    );
}

#[test]
fn the_credential_rides_in_a_config_file_not_argv() {
    if !has("jq") {
        return;
    }
    let c = Case::new("cred-in-file");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "{out}");
    let calls = c.curl_calls();
    assert!(
        calls
            .iter()
            .filter(|l| l.contains("/api/v1/packages/david?type=container"))
            .all(|l| l.ends_with("auth=basic-ok")),
        "the listing did not carry the docker login's exact header: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .filter(|l| l.contains("/manifests/"))
            .all(|l| l.ends_with("auth=bearer-ok")),
        "the manifest reads did not carry the minted bearer: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|l| l.contains("/v2/token") && l.ends_with("auth=basic-ok")),
        "the bearer was not minted through /v2/token with the login: {calls:?}"
    );
    // Every tagged version's index was read, kept or not: a kept tag's
    // children must be known so a shared child is never deleted.
    for tag in ["bdf435d", "deadbee", "latest", "v1"] {
        assert!(
            calls
                .iter()
                .any(|l| l.contains(&format!("/v2/david/boss/manifests/{tag} "))),
            "no index read for {tag}: {calls:?}"
        );
    }
    // A limit is not a filter: the listing walked past the first page.
    assert!(calls.iter().any(|l| l.contains("page=2")), "{calls:?}");
    assert!(calls.iter().any(|l| l.contains("page=3")), "{calls:?}");
}

// ---------------------------------------------------------------------------
// The real run.
// ---------------------------------------------------------------------------

#[test]
fn the_real_run_deletes_exactly_the_planned_set_tags_first_and_records() {
    if !has("jq") {
        return;
    }
    let c = Case::new("for-real");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    no_credential(&out, "real run");
    let deleted = c.deleted();
    let mut expect: Vec<String> = vec![
        "boss 01d0001".into(),
        "boss deadbee".into(),
        format!("boss-ci {}", forty("01d0001")),
        format!("boss {}", digest("old1")),
        format!("boss {}", digest("old2")),
        format!("boss {}", digest("dead1")),
        format!("boss {}", digest("orphan-old")),
        format!("boss-ci {}", digest("ci-old1")),
    ];
    let mut got = deleted.clone();
    // Tags before children: the first three are the tags, in listing
    // order; the children follow.
    assert_eq!(
        &deleted[..3],
        &[
            "boss 01d0001".to_string(),
            "boss deadbee".to_string(),
            format!("boss-ci {}", forty("01d0001"))
        ],
        "tags first: {deleted:?}"
    );
    expect.sort();
    got.sort();
    assert_eq!(got, expect, "the deleted set is not the plan:\n{out}");
    let r = c.record(&out);
    assert_eq!(r["dry_run"], false);
    assert_eq!(r["packages"]["boss"]["deleted"], 6, "{r}");
    assert_eq!(r["packages"]["boss-ci"]["deleted"], 2, "{r}");
    assert_eq!(r["packages"]["boss"]["kept"], 12, "{r}");
    assert_eq!(r["write_scope"], "write:package proven by DELETE", "{r}");
    assert!(r["disk_avail_kb_after"].is_number(), "{r}");
    contains_all(
        &out,
        &["OK", "deleted 8", "df", "Forgejo"],
        "the closing line says what happened and what the bytes wait on",
    );
    // Backlog 1bef55a6 (1): the verdict carries how much of the registry
    // it could judge — the indexes read and the versions left
    // unclassified — so a clean line says what it saw, not only what it
    // did.
    contains_all(
        &out,
        &[
            "OK — deleted 8 of 8 planned version(s)",
            "indexes read 13 of 13 tag(s), 100 percent; 2 unclassified kept",
            "shown gone by a re-list",
        ],
        "the verdict names what it could judge",
    );
    assert_eq!(r["index"]["read_pct"], 100, "{r}");
    assert_eq!(r["relist"]["done"], true, "{r}");
    assert_eq!(r["relist"]["still_listed"], 0, "{r}");
    assert_eq!(r["index"]["bearer"], true, "{r}");
    assert_eq!(r["index"]["tags"], 13, "{r}");
    assert_eq!(r["index"]["read"], 13, "{r}");
    assert_eq!(r["gone"], 0, "{r}");
}

#[test]
fn a_403_on_the_first_delete_refuses_naming_the_scope_and_stops() {
    if !has("jq") {
        return;
    }
    let c = Case::new("forbidden-write");
    write_file(&c.stub.join("delete-code"), "403");
    std::fs::copy(
        c.stub.join("forbidden-write.json"),
        c.stub.join("delete-body"),
    )
    .unwrap();
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["write:package", "403", "David", "Nothing was deleted"],
        "403 on delete",
    );
    no_credential(&out, "403 on delete");
    assert_eq!(
        c.deleted().len(),
        1,
        "a second DELETE was attempted after the refusal: {:?}",
        c.deleted()
    );
    let r = c.record(&out);
    assert_eq!(r["packages"]["boss"]["deleted"], 0, "{r}");
    assert_eq!(r["refused"], "write:package", "{r}");
}

#[test]
fn a_failed_delete_mid_way_stops_and_states_what_was_deleted() {
    if !has("jq") {
        return;
    }
    let c = Case::new("mid-fail");
    write_file(&c.stub.join("delete-fail-on"), "deadbee");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.deleted().len(), 2, "{:?}", c.deleted());
    contains_all(
        &out,
        &["FAILED", "deadbee", "500", "deleted 1"],
        "mid-way failure",
    );
    let r = c.record(&out);
    assert_eq!(r["packages"]["boss"]["deleted"], 1, "{r}");
    assert_eq!(r["packages"]["boss"]["failed"], 1, "{r}");
}

// ---------------------------------------------------------------------------
// The bounds an UNATTENDED run needs (backlog 8d77d670, the adversarial
// review of the daily car): since design 97add747 a rule files this
// delete every day with nobody reading the plan first, so every way the
// keep set can come out smaller than the truth is a refusal here.
// ---------------------------------------------------------------------------

/// The trains, with one OPEN train that references a sha no closed
/// train names (01d0001, old, otherwise deleted) and one that a closed
/// train names too (1111111, contested).
fn trains_with_open_references() -> String {
    serde_json::json!({
        "total": 3,
        "data": [
            {"status": "open", "steps": [
                {"metadata": {"train_ref": "train/2026-09-17-0100@01d0001"}},
                {"metadata": {"car_heads": ["1111111aaaabbbbccccddddeeeeffff000011112"]}}]},
            {"status": "closed", "steps": [{"metadata": {"train_ref": "train/2026-09-17-0000@1111111"}}, {"metadata": {"merge_ref": "222222222222"}}]},
            {"status": "closed", "steps": [{"metadata": {"train_ref": "train/2026-09-16-2300@1111111"}}]},
        ]
    })
    .to_string()
}

/// M3 of the review. `landed_train_shas` answers the DISK SWEEP's
/// question — which images are collectable — so it SUBTRACTS every sha
/// an open train mentions. The prune used that answer as a KEEP set, so
/// an open train made it keep LESS: an in-flight train's image, and a
/// landed one an open train still names, were both deletable. The prune
/// now keeps closed ∪ open.
#[test]
fn an_open_trains_sha_is_kept_by_the_prune() {
    if !has("jq") {
        return;
    }
    let c = Case::new("open-train-kept");
    write_file(&c.stub.join("trains.json"), &trains_with_open_references());
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    let deleted = c.deleted();
    for kept in [
        "boss 01d0001".to_string(),
        format!("boss-ci {}", forty("01d0001")),
        format!("boss {}", digest("old1")),
        "boss 1111111".to_string(),
        format!("boss-ci {}", forty("1111111")),
    ] {
        assert!(
            !deleted.contains(&kept),
            "{kept} belongs to an OPEN train and was deleted: {deleted:?}\n{out}"
        );
    }
    let r = c.record(&out);
    assert_eq!(r["keep"]["landed_trains"], 2, "{r}");
    assert!(
        r["keep"]["open_trains"].as_u64().unwrap_or(0) >= 2,
        "the record does not count the open trains' shas: {r}"
    );
    contains_all(
        &out,
        &["open train"],
        "the kept reason names the open train",
    );
}

/// Both meanings of the one read, pinned side by side: the sweep's
/// COLLECTABLE set (closed minus open) and the prune's KEEP halves
/// (closed, and open) — the lib answers each from the same reply.
#[test]
fn the_train_lookup_answers_collectable_and_keep_from_one_read() {
    if !has("jq") {
        return;
    }
    let c = Case::new("train-sets");
    write_file(&c.stub.join("trains.json"), &trains_with_open_references());
    let work = c.stub.join("sets");
    std::fs::create_dir_all(&work).unwrap();
    let lib = repo_root().join("infra/forge/landed-train-shas.lib.sh");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(
            r#"set -uo pipefail
. "$1"
echo "landed: $(landed_train_shas curl http://sor.invalid:7900 20 t 2>/dev/null | paste -sd ' ')"
train_sha_sets curl http://sor.invalid:7900 20 t "$2/closed" "$2/open" 2>/dev/null; echo "rc: $?"
echo "closed: $(paste -sd ' ' "$2/closed")"
echo "open: $(paste -sd ' ' "$2/open")"
"#,
        )
        .arg("_")
        .arg(&lib)
        .arg(&work);
    c.env(&mut cmd, &[]);
    let out = cmd.output().expect("bash runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = |k: &str| -> Vec<String> {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}: ")))
            .unwrap_or_else(|| panic!("no `{k}:` line in:\n{text}"))
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };
    assert_eq!(line("rc"), vec!["0".to_string()], "{text}");
    // The sweep: the contested sha is NOT collectable.
    assert_eq!(line("landed"), vec!["2222222".to_string()], "{text}");
    // The prune: both closed shas are keep keys, and the open train's.
    assert_eq!(
        line("closed"),
        vec!["1111111".to_string(), "2222222".to_string()],
        "{text}"
    );
    let open = line("open");
    assert!(open.contains(&"01d0001".to_string()), "{text}");
    assert!(open.contains(&"1111111".to_string()), "{text}");
}

/// M1 of the review. An untagged child that no LISTED tag references is
/// an orphan only if the listing was whole: a short listing (fewer
/// versions read than the forge's X-Total-Count) may have missed the tag
/// that owns it, and deleting that child breaks the tag.
///
/// Made a refusal by the re-review of 1bef55a6 (finding 1): the short
/// listing only held the orphans, and the rest was judged on it. With
/// page 1 the newest versions and page 2 empty against a count of 25,
/// the run planned 0, printed `deleted 0 of 0 … 100 percent; 0
/// unclassified` and passed — the read percentage's denominator is the
/// tags LISTED. Now a short listing, or a count that is not a number,
/// deletes nothing and exits 2, and the count is proven digits before
/// anything prints it (finding 3).
#[test]
fn a_listing_short_of_the_forges_count_is_refused() {
    if !has("jq") {
        return;
    }
    // More versions counted than listed.
    let c = Case::new("short-listing");
    write_file(&c.stub.join("total"), "30");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &[
            "REFUSED",
            "the listing read 25 version(s) of the 30",
            "Nothing was deleted",
        ],
        "a short listing",
    );

    // The reviewer's `trunc`: the second page never arrives.
    let c = Case::new("trunc");
    std::fs::remove_file(c.stub.join("page-2.json")).unwrap();
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "a truncated listing passed:\n{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &["REFUSED", "the listing read 20 version(s) of the 25"],
        "trunc",
    );
    assert!(!out.contains("OK —"), "{out}");

    // The reviewer's `inject`: a count that is not a number — here one
    // shaped like the watch's own verdict — is refused, never printed,
    // and nothing in the output reads as a verdict.
    let c = Case::new("inject");
    let injected = "OK x deleted 1 of 1 planned x listed 1 of 1 versions, indexes read 1 of 1 tag(s), 100 percent; 0 unclassified kept";
    write_file(&c.stub.join("total-text"), injected);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    assert!(out.contains("no numeric X-Total-Count"), "{out}");
    assert!(
        !out.contains(injected),
        "the header reached the output:\n{out}"
    );
    assert!(
        watch_groups(&out).is_none(),
        "the watch reads a verdict in:\n{out}"
    );

    // A listing element that is not a version is a failure to read, not
    // a page of fewer rows (re-review, finding 2).
    let c = Case::new("bad-element");
    let p2 = c.stub.join("page-2.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p2).unwrap()).unwrap();
    v.as_array_mut().unwrap().push(serde_json::json!(7));
    write_file(&p2, &v.to_string());
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 1, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    assert!(out.contains("cannot read as a version"), "{out}");
}

/// Re-review finding 2: the re-list had no completeness check and no
/// positive control, so an empty or short re-list — holding none of the
/// planned versions — read `OK — deleted 8 of 8` with nothing deleted.
/// Each lie below fails the run. The forge's count after the deletes is
/// 25 - 8 = 17.
#[test]
fn a_relist_that_cannot_show_the_registry_fails_the_run() {
    if !has("jq") {
        return;
    }
    for (name, file, body, needle) in [
        (
            "relist-empty",
            "relist-empty",
            "",
            "the re-list read 0 version(s) of the 17",
        ),
        (
            "relist-short",
            "relist-short",
            "",
            "the re-list read 14 version(s) of the 17",
        ),
        (
            "relist-bad-element",
            "relist-body",
            "[7]",
            "cannot read as a version",
        ),
        (
            "relist-hides-a-kept-version",
            "relist-hide",
            "boss bdf435d\n",
            "version(s) this run KEPT are missing from the re-list (first: boss:bdf435d)",
        ),
    ] {
        let c = Case::new(name);
        write_file(&c.stub.join(file), body);
        let (rc, out) = c.run(&["--for-real"]);
        assert_eq!(
            rc, 1,
            "{name}: the run passed on a re-list that shows nothing:\n{out}"
        );
        assert!(!out.contains("OK —"), "{name}: an OK verdict:\n{out}");
        contains_all(&out, &["FAILED", "could not be listed again", needle], name);
        assert_eq!(c.record(&out)["relist"]["done"], false, "{name}");
    }
}

/// M1, the other half, also a refusal since the re-review: every keep tag
/// the cluster and the converge name — each live image, the rollback
/// target, and `latest` — must be a LISTED version. One that is not means
/// the listing does not show what is really there, and nothing is judged
/// on it.
#[test]
fn a_listing_missing_a_keep_tag_is_refused() {
    if !has("jq") {
        return;
    }
    let c = Case::new("stamp-unlisted");
    write_file(&c.stamp, "c0ffee1\n");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &[
            "REFUSED",
            "keep tag(s) not in the listing",
            "boss:c0ffee1 (stamp)",
        ],
        "the missing stamp is named",
    );

    let c = Case::new("latest-unlisted");
    let p1 = c.stub.join("page-1.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p1).unwrap()).unwrap();
    v.as_array_mut()
        .unwrap()
        .retain(|e| e["version"] != "latest");
    write_file(&p1, &v.to_string());
    write_file(&c.stub.join("total"), "24");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "{out}");
    assert!(out.contains("boss:latest"), "{out}");

    // The default fixture's listing is whole and names every keep tag.
    let whole = Case::new("listing-whole");
    let (rc, out) = whole.run(&["--dry-run"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("listed 25 of 25 versions"), "{out}");
}

/// M2 of the review: a per-run ceiling. The largest real run deleted
/// 1,337 (2026-09-17, a catch-up); a daily run plans a few hundred. A
/// plan beyond the fixed limit, or beyond a fraction of the two
/// packages' versions, is a keep set that came out wrong more often than
/// a registry that really grew — refused before the first DELETE, naming
/// the numbers.
#[test]
fn a_plan_beyond_the_ceiling_is_refused_before_any_delete() {
    if !has("jq") {
        return;
    }
    // Each refusal names the lift that fits it (backlog 1bef55a6 (3) and
    // its review, findings 3 and 5): past the absolute bound, the hand
    // verb's max_delete at the plan plus 20% (8 -> 10), never the bare
    // plan, which the next push would refuse again; past the fraction, a
    // larger keep_trains, which keeps more and so plans less.
    for (env, value, needles) in [
        (
            "BOSS_PRUNE_MAX_DELETE",
            "5",
            vec![
                "8",
                "5",
                "prune-registry-versions --for-real 10 10",
                "plus 20%",
            ],
        ),
        (
            "BOSS_PRUNE_MAX_PERCENT",
            "20",
            vec![
                "8 of 24",
                "20%",
                "prune-registry-versions --for-real 20",
                "keep_trains",
            ],
        ),
    ] {
        let c = Case::new(&format!("ceiling-{env}"));
        let (rc, out) = c.run_env(&["--for-real"], &[(env, value.to_string())]);
        assert_eq!(rc, 2, "{env}={value}: {out}");
        assert!(c.deleted().is_empty(), "{env}: {:?}", c.deleted());
        let mut all = needles.clone();
        all.extend(["REFUSED", "ceiling", "Nothing was deleted"]);
        contains_all(&out, &all, env);
        // A dry run shows the plan and says a real run would refuse.
        let (rc, out) = c.run_env(&["--dry-run"], &[(env, value.to_string())]);
        assert_eq!(rc, 0, "{env} dry run: {out}");
        contains_all(&out, &["would be REFUSED", "ceiling"], "dry run");
    }
    // The fixed defaults, read off the script: one number each, here.
    let src = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    assert!(src.contains("MAX_DELETE_DEFAULT=1500"), "the limit");
    assert!(
        src.contains("${BOSS_PRUNE_MAX_DELETE:-$MAX_DELETE_DEFAULT}"),
        "the limit's seam"
    );
    assert!(
        src.contains("${BOSS_PRUNE_MAX_PERCENT:-90}"),
        "the fraction"
    );
}

/// Re-review finding 4: the lift a refusal names must WORK. Past both
/// bounds, max_delete would refuse again at the fraction, so the knob
/// named is keep_trains; and a plan of 2,501..3,000, whose plan-plus-20%
/// is past the cap, is named the cap — which lifts it.
#[test]
fn a_ceiling_refusal_names_a_lift_that_works() {
    if !has("jq") {
        return;
    }
    // Past the absolute bound AND the fraction.
    let c = Case::new("ceiling-both");
    let (rc, out) = c.run_env(
        &["--for-real"],
        &[
            ("BOSS_PRUNE_MAX_DELETE", "5".to_string()),
            ("BOSS_PRUNE_MAX_PERCENT", "20".to_string()),
        ],
    );
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &[
            "beyond the ceiling of 5 per run",
            "ALSO past the 20% fraction (8 of 24)",
            "which max_delete does not lift",
            "prune-registry-versions --for-real 20",
        ],
        "past both bounds",
    );
    assert!(
        !out.contains("prune-registry-versions --for-real 10 10"),
        "a max_delete that the fraction would refuse was suggested:\n{out}"
    );

    // A plan of 2,608, under the fraction: 2,600 old orphans (deleted)
    // beside 300 fresh ones (kept) — 2,608 of 2,924 is 89%.
    let c = Case::new("ceiling-2608");
    let p1 = c.stub.join("page-1.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p1).unwrap()).unwrap();
    let (old, fresh) = (hours_ago(5 * 24), hours_ago(2));
    let rows = v.as_array_mut().unwrap();
    for i in 0..2600u64 {
        rows.push(version(
            90_000 + i,
            "boss",
            &digest(&format!("bulk-old-{i}")),
            &old,
        ));
    }
    for i in 0..300u64 {
        rows.push(version(
            95_000 + i,
            "boss",
            &digest(&format!("bulk-new-{i}")),
            &fresh,
        ));
    }
    write_file(&p1, &v.to_string());
    write_file(&c.stub.join("total"), "2925");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &[
            "the plan deletes 2608 version(s), beyond the ceiling of 1500 per run",
            "prune-registry-versions --for-real 10 3000",
        ],
        "a plan of 2608 is named the cap",
    );
    // …and the named lift lifts it (a dry run, so the 2,608 DELETEs are
    // not sent through the stub).
    let (rc, out) = c.run(&["--dry-run", "10", "3000"]);
    assert_eq!(rc, 0, "{out}");
    assert!(!out.contains("would be REFUSED"), "{out}");
    assert!(out.contains("would delete 2608 version(s)"), "{out}");
}

/// Backlog 1bef55a6 (3): the percentage bound GROWS WITH THE BACKLOG.
/// The keep set is roughly constant (ten landed trains, the live image,
/// the rollback target, a day of pushes), so after a gap the plan's
/// share of the registry climbs toward 100% — past 90% after about
/// eight days without a run (ee6caa74 planned 89.6%) — exactly when the
/// registry most needs pruning. And a packet could not lift it: the
/// ceiling came only from BOSS_PRUNE_MAX_*, which the runner never
/// passes. So the HAND verb takes a third argument, `max_delete`: the
/// number of deletions the person filing it accepts, read off a dry run.
///
/// Bounded, not approved (review finding 3). The first cut let it lift
/// the fraction too, up to 99999, for any filer; the review held that.
/// Now it replaces the ABSOLUTE bound only, at most twice its default
/// (3000), and the fraction always holds — its lift is a larger
/// keep_trains, which keeps more and so plans less. It is on the packet
/// (the args), in the record, and it still binds. The daily verb admits
/// no argument, so the unattended run keeps both bounds at default.
#[test]
fn the_hand_verb_names_the_ceiling_it_accepts_in_its_argv() {
    if !has("jq") {
        return;
    }
    // The absolute bound alone would refuse this plan: 8 is past 5.
    let low = [("BOSS_PRUNE_MAX_DELETE", "5".to_string())];
    let c = Case::new("ceiling-argv-lifts");
    let (rc, out) = c.run_env(&["--for-real", "10", "10"], &low);
    assert_eq!(
        rc, 0,
        "a named ceiling the plan fits was not honoured:\n{out}"
    );
    assert_eq!(c.deleted().len(), 8, "{:?}", c.deleted());
    let r = c.record(&out);
    assert_eq!(r["ceiling"]["max_delete"], 10, "{r}");
    assert_eq!(
        r["ceiling"]["max_percent"], 90,
        "the fraction is never lifted: {r}"
    );
    assert_eq!(r["ceiling"]["source"], "argv", "{r}");

    // The fraction holds whatever max_delete names: 8 of 24 is past 20%.
    let c = Case::new("ceiling-argv-fraction-holds");
    let (rc, out) = c.run_env(
        &["--for-real", "10", "3000"],
        &[("BOSS_PRUNE_MAX_PERCENT", "20".to_string())],
    );
    assert_eq!(rc, 2, "max_delete lifted the fraction:\n{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &[
            "REFUSED",
            "8 of 24",
            "20%",
            "keep_trains",
            "prune-registry-versions --for-real 20",
        ],
        "the fraction's lift is keeping more",
    );

    // The named number still binds: a plan past it is refused, and the
    // refusal suggests the plan plus 20%.
    let c = Case::new("ceiling-argv-binds");
    let (rc, out) = c.run(&["--for-real", "10", "5"]);
    assert_eq!(rc, 2, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &[
            "REFUSED",
            "beyond the ceiling of 5 this run's max_delete named",
            "prune-registry-versions --for-real 10 10",
            "Nothing was deleted",
        ],
        "a plan past the named ceiling",
    );

    // The cap is twice the default, and 3000 itself is admitted.
    let c = Case::new("ceiling-argv-cap");
    let (rc, out) = c.run(&["--dry-run", "10", "3000"]);
    assert_eq!(rc, 0, "{out}");

    // Unnamed, the defaults hold and the record says where they came from.
    let c = Case::new("ceiling-default-source");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.record(&out)["ceiling"]["source"], "default", "{out}");

    // The shape and the cap are re-checked by the script, not left to
    // the allowlist.
    for bad in ["0", "08", "ten", "3001", "10000", "100000"] {
        let c = Case::new(&format!("ceiling-argv-bad-{bad}"));
        let (rc, out) = c.run(&["--for-real", "10", bad]);
        assert_eq!(rc, 2, "max_delete {bad:?} was admitted:\n{out}");
        assert!(c.deleted().is_empty(), "{:?}", c.deleted());
        contains_all(&out, &["max_delete", "REFUSED"], bad);
    }
    let c = Case::new("ceiling-argv-too-many");
    let (rc, _) = c.run(&["--for-real", "10", "8", "extra"]);
    assert_eq!(rc, 2, "a fourth argument was admitted");
}

/// L2 of the review: each outcome reaches the list file as it happens,
/// so a run killed mid-way (the runner's timeout) leaves the file saying
/// what went, not only the plan.
#[test]
fn each_outcome_reaches_the_list_file_as_it_happens() {
    if !has("jq") {
        return;
    }
    let c = Case::new("outcomes-streamed");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    let seen = std::fs::read_to_string(c.stub.join("listed-before-delete")).unwrap();
    let counts: Vec<&str> = seen.lines().collect();
    assert_eq!(
        counts,
        vec!["0", "1", "2", "3", "4", "5", "6", "7"],
        "the list file did not grow one outcome per DELETE:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// The verdict comes first, and the full list is a file.
// ---------------------------------------------------------------------------

/// Backlog 5323f3ef (measured 2026-09-17 05:27Z, ops-request 279b8659):
/// 1,313 `would DELETE` lines (165 KB) and the record printed last; the
/// runner kept the first 100 KB, and the packet held no verdict. The
/// order is judged on the streams merged the way the runner merges
/// them, and the record must sit inside the first 4 KB — the window
/// the landing probe reads on the next ops-request.
#[test]
fn the_verdict_precedes_the_per_version_list_and_the_list_is_a_named_file() {
    if !has("jq") {
        return;
    }
    // The per-version verb differs by mode (backlog ea67ad87): a dry
    // run plans, a real run reports.
    for (mode, summary, delete_verb, unclassified_line) in [
        (
            "--dry-run",
            "DRY RUN",
            "would DELETE ",
            "unclassified boss:v1",
        ),
        (
            "--for-real",
            "OK —",
            "DELETED ",
            "KEPT boss:v1 — unclassified",
        ),
    ] {
        let c = Case::new(&format!("verdict-first{mode}"));
        let (rc, out) = c.run_combined(&[mode]);
        assert_eq!(rc, 0, "{mode}: {out}");
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{mode}: no `{needle}` in:\n{out}"))
        };
        let record_at = at(r#"{"verb":"prune-registry-versions""#);
        let first_delete = at(delete_verb);
        let first_unclassified = at(unclassified_line);
        let summary_at = at(summary);
        assert!(
            record_at < first_delete && record_at < first_unclassified,
            "{mode}: the record follows a per-version line (record at {record_at}, \
             first {delete_verb} at {first_delete}, first unclassified at {first_unclassified}):\n{out}"
        );
        assert!(
            summary_at < first_delete,
            "{mode}: the summary line follows the per-version list:\n{out}"
        );
        assert!(
            record_at < 4096,
            "{mode}: the record starts at byte {record_at}, outside the first 4 KB:\n{out}"
        );
        // Every per-version line is still printed, after the verdict.
        contains_all(
            &out,
            &[
                &format!("{delete_verb}boss:01d0001"),
                &format!("{delete_verb}boss-ci:{}", digest("ci-old1")),
                unclassified_line,
            ],
            mode,
        );

        // The record names the list file, under the directory the verb
        // was given, and the file holds every per-version line.
        let r = c.record(&out);
        let list_file = PathBuf::from(
            r["list_file"]
                .as_str()
                .unwrap_or_else(|| panic!("{mode}: the record names no list_file: {r}")),
        );
        assert!(
            list_file.starts_with(&c.list_dir),
            "{mode}: {} is not under {}",
            list_file.display(),
            c.list_dir.display()
        );
        assert_eq!(list_file.extension().and_then(|e| e.to_str()), Some("txt"));
        let list = std::fs::read_to_string(&list_file)
            .unwrap_or_else(|e| panic!("{mode}: {} unreadable: {e}", list_file.display()));
        contains_all(
            &list,
            &[
                mode,
                "would DELETE boss:01d0001",
                "would DELETE boss:deadbee",
                &format!("would DELETE boss:{}", digest("orphan-old")),
                &format!("would DELETE boss-ci:{}", forty("01d0001")),
                "unclassified boss:v1",
                "unclassified boss-ci:rust1.96",
            ],
            "the list file",
        );
        no_credential(&list, "the list file");
        if mode == "--for-real" {
            // The outcomes ride the same file: what went, by name.
            contains_all(
                &list,
                &["deleted boss:01d0001", "deleted boss:deadbee"],
                "the list file's outcomes",
            );
        } else {
            assert!(!list.contains("deleted boss"), "a dry run deleted: {list}");
        }
    }
}

/// Backlog ea67ad87 (measured on the first real prune, ops-request
/// 973beaa2, 2026-09-17 13:41Z, deleted 1,337 of 1,337): the verdict
/// said `OK — deleted`, and every per-version line after it — the 898
/// the runner kept — read `would DELETE <tag> — older than the keep
/// set`, because the plan file was printed as-is and the outcomes were
/// appended after the runner had cut the output. A real run described
/// in the conditional. Now a real run's per-version line carries its
/// OUTCOME as the verb — DELETED / GONE / FAILED <code> / KEPT — and
/// only a dry run says `would DELETE`. The file on disk is unchanged:
/// the plan, then the outcomes appended after the deletes.
#[test]
fn a_real_run_prints_each_planned_version_with_its_outcome_as_the_verb() {
    if !has("jq") {
        return;
    }
    let c = Case::new("outcome-verbs");
    let (rc, out) = c.run_combined(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        !out.contains("would DELETE"),
        "a real run printed a conditional per-version line:\n{out}"
    );
    contains_all(
        &out,
        &[
            "DELETED boss:01d0001 — older than the keep set",
            "DELETED boss:deadbee — older than the keep set",
            &format!(
                "DELETED boss:{} — child of a deleted tag (01d0001)",
                digest("old1")
            ),
            &format!(
                "DELETED boss:{} — orphan: referenced by no tag",
                digest("orphan-old")
            ),
            &format!(
                "DELETED boss-ci:{} — older than the keep set",
                forty("01d0001")
            ),
            "KEPT boss:v1 — unclassified: tag is not a sha",
            "KEPT boss-ci:rust1.96 — unclassified: tag is not a sha",
        ],
        "the real run's per-version lines",
    );
    // Still in the runner's order: the record, then the verdict, then
    // the first outcome line.
    let at = |needle: &str| {
        out.find(needle)
            .unwrap_or_else(|| panic!("no `{needle}` in:\n{out}"))
    };
    let record_at = at(r#"{"verb":"prune-registry-versions""#);
    let verdict_at = at("OK —");
    let first_outcome = at("DELETED ");
    assert!(
        record_at < verdict_at && verdict_at < first_outcome,
        "record at {record_at}, verdict at {verdict_at}, first DELETED at {first_outcome}:\n{out}"
    );
    // The file keeps the plan and the appended outcomes, as before.
    let r = c.record(&out);
    let list = std::fs::read_to_string(r["list_file"].as_str().unwrap()).unwrap();
    contains_all(
        &list,
        &[
            "would DELETE boss:01d0001 — older than the keep set",
            "deleted boss:01d0001",
        ],
        "the list file",
    );

    // A dry run is unchanged: the conditional, and no outcome verb.
    let c = Case::new("outcome-verbs-dry");
    let (rc, out) = c.run_combined(&["--dry-run"]);
    assert_eq!(rc, 0, "{out}");
    contains_all(
        &out,
        &[
            "would DELETE boss:01d0001 — older than the keep set",
            "unclassified boss:v1 — tag is not a sha (kept)",
        ],
        "the dry run's per-version lines",
    );
    assert!(
        !out.contains("DELETED ") && !out.contains("KEPT "),
        "a dry run claimed an outcome:\n{out}"
    );
}

/// A run that stops part-way names the failure on the version's own
/// line, with the code, and every planned version it never reached
/// reads KEPT with the reason it was not attempted — not `would
/// DELETE`, which would be the same conditional the packet above
/// misread.
#[test]
fn a_stopped_real_run_says_failed_with_the_code_and_kept_for_the_rest() {
    if !has("jq") {
        return;
    }
    let c = Case::new("outcome-verbs-fail");
    write_file(&c.stub.join("delete-fail-on"), "deadbee");
    let (rc, out) = c.run_combined(&["--for-real"]);
    assert_eq!(rc, 1, "{out}");
    assert!(!out.contains("would DELETE"), "{out}");
    contains_all(
        &out,
        &[
            "DELETED boss:01d0001 — older than the keep set",
            "FAILED 500 boss:deadbee — older than the keep set",
            &format!(
                "KEPT boss-ci:{} — older than the keep set (not attempted: the run stopped)",
                forty("01d0001")
            ),
            &format!(
                "KEPT boss:{} — child of a deleted tag (01d0001) (not attempted: the run stopped)",
                digest("old1")
            ),
        ],
        "the stopped run's per-version lines",
    );
}

/// The second observation on ea67ad87: df before/after moved 724 KB on
/// 1,337 deletions — the blobs are freed by Forgejo's own
/// `[cron.cleanup_packages]`, so the disk alarm clears only when that
/// runs. Its app.ini is NOT in this tree (the forge's compose file and
/// data directory are unversioned, infra/forge/OPERATIONS.md), so the
/// verb reads the cadence off the host's app.ini at run time — the
/// same file under the data dir that publish-github-pr.sh reads
/// `[repository] ROOT` from — and the record says what it found and
/// where, or that it could not read it. Never a value typed here.
#[test]
fn the_record_carries_forgejo_cleanup_cadence_read_off_app_ini_or_says_it_could_not() {
    if !has("jq") {
        return;
    }
    // Set in app.ini: the record and the verdict carry the values.
    let c = Case::new("cleanup-cron-set");
    let conf = c.df_path.join("gitea/conf");
    std::fs::create_dir_all(&conf).unwrap();
    write_file(
        &conf.join("app.ini"),
        "[repository]\nROOT = /data/git/repositories\n\n[cron.cleanup_packages]\nENABLED = true\nRUN_AT_START = false\nSCHEDULE = @every 6h\nOLDER_THAN = 48h\n\n[cron.other]\nSCHEDULE = @weekly\n",
    );
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    let r = c.record(&out);
    let cron = &r["cleanup_cron"];
    assert_eq!(cron["schedule"], "@every 6h", "{r}");
    assert_eq!(cron["older_than"], "48h", "{r}");
    assert_eq!(cron["enabled"], "true", "{r}");
    assert_eq!(
        cron["source"],
        conf.join("app.ini").to_string_lossy().as_ref(),
        "{r}"
    );
    contains_all(
        &out,
        &["cleanup_packages", "@every 6h", "48h"],
        "the verdict names the cadence it read",
    );

    // Readable, section absent: said as unset, with Forgejo's default
    // named as documented, not as read.
    let c = Case::new("cleanup-cron-unset");
    let conf = c.df_path.join("gitea/conf");
    std::fs::create_dir_all(&conf).unwrap();
    write_file(
        &conf.join("app.ini"),
        "[repository]\nROOT = /data/git/repositories\n",
    );
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "{out}");
    let r = c.record(&out);
    let cron = &r["cleanup_cron"];
    assert!(cron["schedule"].is_null(), "{r}");
    assert!(cron["older_than"].is_null(), "{r}");
    assert!(
        cron["note"]
            .as_str()
            .is_some_and(|n| n.contains("unset") && n.contains("@midnight")),
        "{r}"
    );

    // No app.ini where the data dir says it should be: said, not
    // guessed.
    let c = Case::new("cleanup-cron-unread");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    let r = c.record(&out);
    let cron = &r["cleanup_cron"];
    assert!(cron["schedule"].is_null(), "{r}");
    assert!(
        cron["source"]
            .as_str()
            .is_some_and(|s| s.starts_with("unread: ") && s.contains("app.ini")),
        "{r}"
    );
    contains_all(
        &out,
        &["unread"],
        "the verdict says the cadence was not read",
    );
}

/// The file is written BEFORE the first DELETE, and a file that cannot
/// be written is a refusal: a real run whose only full record is the
/// runner's 100 KB window would be the defect again.
#[test]
fn an_unwritable_list_file_refuses_before_any_delete() {
    if !has("jq") {
        return;
    }
    let c = Case::new("list-unwritable");
    // The list "directory" is a plain file, so it cannot be created.
    write_file(&c.list_dir, "not a directory\n");
    for mode in ["--dry-run", "--for-real"] {
        let (rc, out) = c.run(&[mode]);
        assert_eq!(rc, 2, "{mode}: {out}");
        contains_all(
            &out,
            &["REFUSED", "registry-prune", "Nothing was deleted"],
            mode,
        );
        assert!(
            c.deleted().is_empty(),
            "{mode}: a DELETE was sent without the list file: {:?}",
            c.deleted()
        );
        no_credential(&out, mode);
    }
}

// ---------------------------------------------------------------------------
// Unclassified is kept.
// ---------------------------------------------------------------------------

#[test]
fn a_tag_whose_index_cannot_be_read_is_kept_and_so_are_all_orphans() {
    if !has("jq") {
        return;
    }
    let c = Case::new("index-unreadable");
    std::fs::remove_file(c.stub.join("index-boss-deadbee.json")).unwrap();
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    let deleted = c.deleted();
    assert!(
        !deleted.iter().any(|d| d == "boss deadbee"),
        "a tag whose children are unknown was deleted: {deleted:?}"
    );
    assert!(
        !deleted.iter().any(|d| d.contains("orphan-old")),
        "an orphan was deleted while an index was unreadable: {deleted:?}"
    );
    // dead1 is a child of deadbee only; with that index unread it is an
    // orphan, and orphans are unclassified this pass.
    assert!(
        !deleted.iter().any(|d| d.contains(&digest("dead1"))),
        "{deleted:?}"
    );
    // 01d0001 and its children are still classified and go.
    assert!(deleted.iter().any(|d| d == "boss 01d0001"), "{deleted:?}");
    assert!(
        deleted.iter().any(|d| d.contains(&digest("old1"))),
        "{deleted:?}"
    );
    // A real run's line is the outcome (backlog ea67ad87): kept, and why.
    contains_all(
        &out,
        &[
            "KEPT boss:deadbee — unclassified: index unreadable (HTTP 404)",
            "KEPT boss:sha256:",
            "unclassified: referenced by no readable index while 1 index(es) were unreadable",
        ],
        "the unreadable index is named",
    );
    let r = c.record(&out);
    assert_eq!(r["packages"]["boss"]["index_unreadable"], 1, "{r}");
    assert_eq!(r["packages"]["boss"]["unclassified"], 4, "{r}");
}

/// Backlog 1bef55a6 (1), the review of the daily car: with no bearer
/// every tag's children are unknown and every orphan is held, so the
/// plan is ZERO by construction — and until this the run said `OK —
/// deleted 0 of 0 planned` and exited 0, which the watch's `deleted =
/// planned` passes. Every night would pass while the registry grew ~9 GB
/// a day. A run that could read no index judged nothing: it still
/// prints its record (the unclassified list is the evidence), and then
/// FAILS, naming why, so the watch files its alarm off the exit.
#[test]
fn no_bearer_means_no_index_is_readable_and_the_run_fails_rather_than_passing() {
    if !has("jq") {
        return;
    }
    for mode in ["--for-real", "--dry-run"] {
        let c = Case::new(&format!("no-bearer{mode}"));
        write_file(&c.stub.join("no-token"), "");
        let (rc, out) = c.run(&[mode]);
        assert_eq!(
            rc, 1,
            "{mode}: a run that could read no index answered as if it had judged the registry:\n{out}"
        );
        assert!(c.deleted().is_empty(), "{:?}", c.deleted());
        contains_all(
            &out,
            &[
                "/v2/token",
                "401",
                "FAILED",
                "no manifest index could be read",
                "indexes read 0 of 13",
                "no bearer",
            ],
            "the token refusal is named, as a failure",
        );
        assert!(!out.contains("OK —"), "{mode}: an OK verdict:\n{out}");
        assert!(!out.contains("DRY RUN —"), "{mode}: a plan verdict:\n{out}");
        let r = c.record(&out);
        assert_eq!(r["packages"]["boss"]["deleted"], 0, "{r}");
        assert_eq!(r["packages"]["boss"]["delete"], 0, "{r}");
        assert_eq!(r["index"]["bearer"], false, "{r}");
        assert_eq!(r["index"]["tags"], 13, "{r}");
        assert_eq!(r["index"]["read"], 0, "{r}");
    }
}

/// The same zero plan reached the other way: a bearer is minted, and
/// every manifest read fails (a scope the bearer lacks, a registry that
/// answers 404 to all of them). No index read is no index read.
#[test]
fn a_bearer_that_reads_no_index_fails_the_run_too() {
    if !has("jq") {
        return;
    }
    let c = Case::new("no-index-read");
    for entry in std::fs::read_dir(&c.stub).unwrap() {
        let p = entry.unwrap().path();
        if p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("index-"))
        {
            std::fs::remove_file(p).unwrap();
        }
    }
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 1, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &[
            "FAILED",
            "no manifest index could be read",
            "indexes read 0 of 13",
            "every manifest read failed",
        ],
        "every index unreadable",
    );
    let r = c.record(&out);
    assert_eq!(r["index"]["bearer"], true, "{r}");
    assert_eq!(r["index"]["read"], 0, "{r}");
}

/// Backlog 1bef55a6 (LOW): a version another run deleted first answers
/// 404 — it is GONE, which is what the plan asked for. Counted apart
/// from `deleted` in the record, it made the verdict read `deleted 7 of
/// 8 planned`, and the watch's `deleted = planned` filed an urgent alarm
/// for a run that did its whole job. It is done because the RE-LIST no
/// longer holds it, not because its DELETE said 404; the verdict says
/// how many answered 404, and the record keeps the two apart.
#[test]
fn a_version_already_gone_counts_as_done_in_the_verdict() {
    if !has("jq") {
        return;
    }
    let c = Case::new("gone");
    write_file(&c.stub.join("delete-gone-on"), &digest("orphan-old"));
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    contains_all(
        &out,
        &[
            "OK — deleted 8 of 8 planned version(s)",
            "shown gone by a re-list",
            "1 of them already gone (404)",
            &format!("GONE boss:{}", digest("orphan-old")),
        ],
        "a concurrent 404 is done",
    );
    let r = c.record(&out);
    assert_eq!(r["deleted"], 7, "{r}");
    assert_eq!(r["gone"], 1, "{r}");
    assert_eq!(r["packages"]["boss"]["gone"], 1, "{r}");
    assert_eq!(r["relist"]["done"], true, "{r}");
    assert_eq!(r["relist"]["still_listed"], 0, "{r}");
}

/// Review finding 1 of backlog 1bef55a6 — BLOCKING, and a regression
/// the first cut made: with every DELETE answering 404 (a moved Forgejo
/// route answers that to everything) the run said `OK — deleted 8 of 8`
/// and exited 0 while the record said deleted 0, gone 8. A 404 is the
/// forge's claim, not an effect: the run lists the registry again, the
/// eight are all still there, and it fails, naming the answer.
#[test]
fn a_run_whose_every_delete_answers_404_fails() {
    if !has("jq") {
        return;
    }
    let c = Case::new("all-404");
    write_file(&c.stub.join("delete-code"), "404");
    std::fs::copy(c.stub.join("notfound.json"), c.stub.join("delete-body")).unwrap();
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(
        rc, 1,
        "every DELETE answered 404 and the run passed:\n{out}"
    );
    assert!(!out.contains("OK —"), "an OK verdict:\n{out}");
    contains_all(
        &out,
        &[
            "FAILED",
            "every one of the 8 planned DELETEs answered 404",
            "the re-list still holds 8",
            "STILL LISTED boss:01d0001",
            "(its DELETE answered 404)",
        ],
        "all 404",
    );
    let r = c.record(&out);
    assert_eq!(r["deleted"], 0, "{r}");
    assert_eq!(r["gone"], 8, "{r}");
    assert_eq!(r["relist"]["still_listed"], 8, "{r}");
}

/// A DELETE that answered 204 and removed nothing: the re-list still
/// holds the version, and the run fails naming it — whatever the answer.
#[test]
fn a_planned_version_the_relist_still_holds_fails_the_run() {
    if !has("jq") {
        return;
    }
    let c = Case::new("survivor");
    write_file(&c.stub.join("delete-survives"), &digest("orphan-old"));
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 1, "a survivor passed:\n{out}");
    assert!(!out.contains("OK —"), "an OK verdict:\n{out}");
    contains_all(
        &out,
        &[
            "FAILED",
            "still holds 1 of the 8 planned version(s)",
            "7 are shown gone",
            &format!(
                "STILL LISTED boss:{} — orphan: referenced by no tag (its DELETE answered 204)",
                digest("orphan-old")
            ),
        ],
        "a survivor is named",
    );
    let r = c.record(&out);
    assert_eq!(r["deleted"], 8, "the DELETEs all answered 204: {r}");
    assert_eq!(r["relist"]["still_listed"], 1, "{r}");
}

/// Review finding 2: a pass that read one index of thirteen planned
/// nothing and said `OK — deleted 0 of 0`. Under 90% read, it fails.
#[test]
fn a_pass_that_read_under_ninety_percent_of_its_indexes_fails() {
    if !has("jq") {
        return;
    }
    let c = Case::new("one-index");
    for entry in std::fs::read_dir(&c.stub).unwrap() {
        let p = entry.unwrap().path();
        let n = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if n.starts_with("index-") && n != "index-boss-latest.json" {
            std::fs::remove_file(p).unwrap();
        }
    }
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 1, "{out}");
    assert!(!out.contains("OK —"), "an OK verdict:\n{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &[
            "FAILED",
            "too few manifest indexes could be read",
            "indexes read 1 of 13",
            "7 percent read, under the 90 percent floor",
        ],
        "one index of thirteen",
    );
    assert_eq!(c.record(&out)["index"]["read_pct"], 7);
}

/// The daily watch's own pattern, off its rule file — the regex
/// `ops.judge` reads the verdict with (the tag_release_sh precedent: one
/// fact, two files, compiled here and run over the script's real
/// output). Returns the groups of the last matching line.
fn watch_groups(out: &str) -> Option<std::collections::BTreeMap<String, String>> {
    let path =
        boss_testing::dispatcher_rules_dir().join("watch-prune-registry-versions-daily.toml");
    let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let src = doc["rule"][0]["do"][0]["args"]["verdict_pattern"]
        .as_str()
        .expect("verdict_pattern");
    let inner = src
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("not an expr string literal: {src}"));
    let re = regex::Regex::new(inner).unwrap();
    let line = out.lines().map(str::trim).rfind(|l| re.is_match(l))?;
    let caps = re.captures(line)?;
    Some(
        re.capture_names()
            .flatten()
            .filter_map(|n| Some((n.to_string(), caps.name(n)?.as_str().to_string())))
            .collect(),
    )
}

/// The clean line carries, in the shape the watch captures, what it
/// could judge — and the groups are what the dispatcher test's `when`
/// is evaluated over (prune_registry_versions_daily_rule.rs).
#[test]
fn the_watch_captures_what_the_run_could_judge_from_its_real_line() {
    if !has("jq") {
        return;
    }
    let c = Case::new("watch-groups");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    let g = watch_groups(&out).unwrap_or_else(|| panic!("the watch reads no verdict in:\n{out}"));
    assert_eq!(g["deleted"], "8");
    assert_eq!(g["planned"], "8");
    assert_eq!(g["listed"], "25");
    assert_eq!(g["total"], "25");
    assert_eq!(g["read_pct"], "100");
    assert_eq!(g["unclassified"], "2");
    // Anchored at the line's start (re-review finding 3): the same text
    // behind any other prefix is no verdict.
    let verdict = out
        .lines()
        .find(|l| l.starts_with("prune-registry-versions: OK — "))
        .expect("the OK line");
    let elsewhere = format!("prune-registry-versions: registry: the forge counts {verdict}");
    assert!(watch_groups(&elsewhere).is_none(), "{elsewhere}");
}

/// Review finding 2's second case: every created_at in a shape `date`
/// cannot parse. Every index reads and the listing is whole, but every
/// version is held unclassified and the plan is 0. The first review
/// asked the watch to refuse `planned 0 with unclassified > 0`; the
/// re-review (finding 5) dropped that clause, because the live registry
/// always holds permanent non-sha tags (rust1.96) and it would alarm on
/// every quiet day. So this reads, and the watch passes, as a whole
/// listing that planned nothing — the groups are pinned here so that
/// choice stays visible; `when` over them is pinned beside the rule.
#[test]
fn an_unparseable_created_at_plans_nothing_and_the_watch_reads_it_so() {
    if !has("jq") {
        return;
    }
    let c = Case::new("bad-created-at");
    for page in ["page-1.json", "page-2.json"] {
        let p = c.stub.join(page);
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        for row in v.as_array_mut().unwrap() {
            row["created_at"] = serde_json::json!("27/09/2026 03:00 PDT");
        }
        write_file(&p, &v.to_string());
    }
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    let g = watch_groups(&out).unwrap_or_else(|| panic!("the watch reads no verdict in:\n{out}"));
    assert_eq!(g["planned"], "0", "{out}");
    assert_eq!(g["read_pct"], "100", "{out}");
    assert_eq!((g["listed"].as_str(), g["total"].as_str()), ("25", "25"));
    let unclassified: u32 = g["unclassified"].parse().unwrap();
    assert!(unclassified > 0, "{out}");
    assert!(out.contains("created_at does not parse"), "{out}");
}

// ---------------------------------------------------------------------------
// The verb file and the §9a facts.
// ---------------------------------------------------------------------------

#[test]
fn the_verb_file_is_a_mutating_forge_verb_with_mode_and_an_optional_keep_count() {
    let path = repo_root().join("infra/ops/verbs/prune-registry-versions.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}", "{2}", "{3}"]));
    let params = v["params"].as_array().unwrap();
    assert_eq!(params.len(), 3);
    // Backlog 1bef55a6 (3): the ceiling a hand run accepts, named on the
    // packet. Optional (absent, the script's defaults hold), and its
    // shape is the script's own re-check: 1..3000, no leading zero.
    assert_eq!(params[2]["name"], "max_delete");
    assert_eq!(params[2]["optional"], true);
    assert_eq!(params[0]["name"], "mode");
    assert_eq!(
        params[0]["one_of"],
        serde_json::json!(["--dry-run", "--for-real"])
    );
    assert_eq!(params[1]["name"], "keep_trains");
    assert_eq!(params[1]["optional"], true);
    if has("jq") {
        let matches = |pat: &str, value: &str| -> bool {
            let out = Command::new("jq")
                .args([
                    "-n",
                    "--arg",
                    "v",
                    value,
                    "--arg",
                    "p",
                    pat,
                    "$v | test($p)",
                ])
                .output()
                .expect("jq runs");
            String::from_utf8_lossy(&out.stdout).trim() == "true"
        };
        let pat = params[1]["pattern"].as_str().unwrap();
        for ok in ["1", "10", "250"] {
            assert!(matches(pat, ok), "keep pattern refuses {ok}");
        }
        for bad in ["0", "01", "-1", "1000", "ten", "1 0", ""] {
            assert!(!matches(pat, bad), "keep pattern admits {bad:?}");
        }
        let pat = params[2]["pattern"].as_str().unwrap();
        for ok in ["1", "1500", "3000"] {
            assert!(matches(pat, ok), "max_delete pattern refuses {ok}");
        }
        for bad in ["0", "08", "10000", "-5", "all", ""] {
            assert!(!matches(pat, bad), "max_delete pattern admits {bad:?}");
        }
        // The runner's numeric ceiling is the script's cap: twice the
        // default absolute bound (review finding 3).
        assert_eq!(params[2]["max"], 3000, "{}", params[2]);
    }
    let about = v["about"].as_str().unwrap();
    assert!(about.starts_with("MUTATING"), "{about}");
    assert!(about.contains("David"), "about names who authorized it");
    assert!(about.contains("9789a827"), "about names the packet");
    assert!(about.contains("--dry-run"), "about names the rehearsal");
    // Backlog ea67ad87: a real run's lines carry the outcome, and the
    // cleanup cadence is read off the forge's app.ini, not typed.
    assert!(
        about.contains("ea67ad87"),
        "about names the outcome-verb packet"
    );
    assert!(about.contains("DELETED"), "about names the real run's verb");
    assert!(
        about.contains("cleanup_cron"),
        "about names the record's cadence field"
    );
    assert!(
        about.contains("read:package") && about.contains("write:package"),
        "about names the scopes"
    );
    assert!(
        about.contains("max_delete") && about.contains("1bef55a6"),
        "about names the ceiling a hand run may name, and why"
    );
    // ~1,400 versions: one listing page per 50, one manifest read per
    // tag, one DELETE per version — minutes, not the runner's 30 s.
    assert!(v["timeout"].as_i64().unwrap() >= 600, "{}", v["timeout"]);
    let script = repo_root().join(SCRIPT);
    use std::os::unix::fs::PermissionsExt;
    assert!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o111 != 0);
}

/// The daily twin (backlog 8d77d670, design 97add747): the same script
/// with `--for-real` FIXED in its argv, so the delete a dispatcher rule
/// files is a verb of its own that can only be that delete (a rule COULD
/// pass the hand verb an args list since 4d53fae2; the daily rule sets
/// none). It is a MUTATING verb run with no human filing it, so
/// its `about` must carry the authorisation the verbs README requires:
/// David, the design that asked, and the day he answered it. The rule
/// half is pinned in
/// crates/core/boss-dispatcher/tests/prune_registry_versions_daily_rule.rs.
#[test]
fn the_daily_verb_is_the_real_run_with_its_authorisation_on_record() {
    let path = repo_root().join("infra/ops/verbs/prune-registry-versions-daily.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "--for-real"]));
    assert_eq!(v["params"], serde_json::json!([]));
    let about = v["about"].as_str().unwrap();
    assert!(about.starts_with("MUTATING"), "{about}");
    for needle in [
        "David",
        "2026-09-27",
        "97add747",
        "8d77d670",
        "prune-registry-versions-daily",
        "keep_trains",
    ] {
        assert!(
            about.contains(needle),
            "about does not name {needle}: {about}"
        );
    }
    assert!(v["timeout"].as_i64().unwrap() >= 600, "{}", v["timeout"]);
}

/// §9a: the registry and the stamp file the script defaults to are the
/// converge's — the rollback target is read from the same file the
/// deploy runner writes (and the watchdog rolls to by name; that
/// script is not read here because the gate's scope self-test holds it
/// up as an example of a file no crate reads).
#[test]
fn the_registry_and_the_stamp_are_the_ones_the_converge_declares() {
    let src = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let runner =
        std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let want = |s: &str| assert!(src.contains(s), "script lacks `{s}`");
    // Since 2026-09-18 (backlog 5222163e) neither script spells the
    // image repo: both source forge-defaults.sh and ask it for REGISTRY,
    // and the stamp's file name is that file's LAST_BUILT_NAME.
    want("forge-defaults.sh");
    want("forge_need REGISTRY");
    assert!(runner.contains("forge-defaults.sh") && runner.contains("forge_need REGISTRY"));
    want("${BOSS_FORGE_LAST_BUILT:-");
    want("$LAST_BUILT_NAME");
    let defaults =
        std::fs::read_to_string(repo_root().join("infra/forge/forge-defaults.sh")).unwrap();
    assert!(defaults.contains("LAST_BUILT_NAME=\".boss-last-built\""));
    assert!(
        defaults.contains("STAMP_FILE=\"${BOSS_FORGE_LAST_BUILT:-${HOME:-}/$LAST_BUILT_NAME}\"")
    );
    // The landed shas come from the one lib the sweep reads, not a copy.
    want("landed-train-shas.lib.sh");
    // Its halves, not the sweep's difference (backlog 8d77d670, M3):
    // a keep set adds what open trains carry.
    want("train_sha_sets ");
    assert!(
        !src.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("landed_train_shas ")),
        "the prune reads the sweep's collectable set (closed minus open) as a keep set"
    );
    // No trace: `set -x` would print the curl config's header.
    assert!(
        !src.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("set -x")),
        "the script traces (set -x) — a trace prints the credential"
    );
    // The ops runner runs verbs as root with no HOME; the docker login
    // is the checkout owner's, read off the directory, never $HOME.
    assert!(
        !src.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("$HOME")),
        "the script reads $HOME (the ops runner has none)"
    );
}
