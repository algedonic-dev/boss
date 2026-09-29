//! `infra/forge/credential-render.sh` — the forge host renders the DR
//! copy's GitHub App installation token from the broker's Secret into the
//! root-only file the off-site push reads, on EVERY forge-converge tick
//! and BEFORE the push (design 76155676, backlog 81eb6d4d).
//!
//! WHY. An installation token lives one hour; the broker re-mints it into
//! Secret boss/github-dr-push-token on a clock. A file filled once is dead
//! within the hour, and a file holding a dead token turns a missing
//! credential into a confusing push failure. So every tick leaves the
//! file holding a LIVE token or NOT THERE — and the push refuses an empty
//! slot loudly (its exit 4), never pushing with a dead token and never
//! skipping in silence.
//!
//! HOW THIS IS MEASURED. The script runs for real against the tree's own
//! broker rule and a stub `kubectl` that answers the Secret read from
//! files in scratch, base64-encoding them the way the API server does and
//! answering NotFound for a Secret with no directory. Fixture tokens are
//! fake and assembled here; every case also checks that the token never
//! reaches the script's output.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/forge/credential-render.sh";
const RULE: &str = "infra/dispatcher/rules/broker-rotates-the-github-dr-push-token.toml";
const CONVERGE: &str = "infra/forge/forge-converge.sh";
const OFFSITE_JSON: &str = "infra/forge/offsite-push.json";

// Shaped like an installation token, fake.
const TOKEN: &str = "ghs_fixture0000000000000000000000render01";
const NEWER: &str = "ghs_fixture1111111111111111111111render02";

/// The stub kubectl: the one copy in boss-testing, shared with
/// github_act_sh.rs, which drives this render for real (backlog 4ce4ec55).
const STUB_KUBECTL: &str = boss_testing::kubectl_secret_stub::SECRET_KUBECTL;

struct Case {
    root: PathBuf,
    secrets: PathBuf,
    dest: PathBuf,
    kubectl: PathBuf,
    /// The Secret the rule under test declares: the DR token's, or the
    /// per-request admin token's (`Case::per_request`).
    secret_name: &'static str,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("credential-render-{name}"));
        let secrets = root.join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let kubectl = root.join("kubectl");
        write_exec(&kubectl, STUB_KUBECTL);
        Self {
            root: root.clone(),
            secrets,
            dest: root.join("boss-publish").join("github-dr.token"),
            kubectl,
            secret_name: "github-dr-push-token",
        }
    }

    /// A case over the algedonic-dev admin token's Secret, which the
    /// broker keys per ops-request (backlog 4ce4ec55).
    fn per_request(name: &str) -> Self {
        let c = Self::new(name);
        Self {
            dest: c.root.join("github-app").join("algedonic-dev.token"),
            secret_name: "github-app-algedonic-dev",
            ..c
        }
    }

    fn secret_dir(&self) -> PathBuf {
        self.secrets.join("boss").join(self.secret_name)
    }

    /// The Secret as the broker leaves it: the object exists, and holds
    /// each key given.
    fn secret(&self, entries: &[(&str, &str)]) {
        let dir = self.secret_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for (k, v) in entries {
            std::fs::write(dir.join(k), v).unwrap();
        }
    }

    fn held(&self, token: &str, expires: &str) {
        std::fs::create_dir_all(self.dest.parent().unwrap()).unwrap();
        std::fs::write(&self.dest, token).unwrap();
        std::fs::write(self.expires_file(), expires).unwrap();
    }

    /// A slot as a render for `request` left it: the token, its expiry,
    /// and the request it was rendered for beside them.
    fn held_for(&self, token: &str, expires: &str, request: &str) {
        self.held(token, expires);
        std::fs::write(self.request_file(), request).unwrap();
    }

    fn expires_file(&self) -> PathBuf {
        PathBuf::from(format!("{}.expires-at", self.dest.display()))
    }

    fn request_file(&self) -> PathBuf {
        PathBuf::from(format!("{}.request", self.dest.display()))
    }

    fn run_rule(&self, rule: &str) -> (i32, String) {
        self.run_args(rule, &[])
    }

    /// `--expire <file>`: the act that used `used` is over.
    fn run_expire(&self, used: &str) -> (i32, String) {
        let f = self.root.join("used.token");
        std::fs::write(&f, format!("{used}\n")).unwrap();
        self.run_args(RULE, &["--expire", &f.display().to_string()])
    }

    fn run_args(&self, rule: &str, extra: &[&str]) -> (i32, String) {
        let out = Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .args(["--rule", &repo_root().join(rule).display().to_string()])
            .args(["--dest", &self.dest.display().to_string()])
            .args(extra)
            .env("BOSS_RENDER_KUBECTL", &self.kubectl)
            .env("STUB_SECRETS", &self.secrets)
            .env_remove("BOSS_RUN_SUMMARY_FILE")
            .output()
            .expect("run credential-render.sh");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        for t in [TOKEN, NEWER] {
            assert!(!text.contains(t), "a token reached the output:\n{text}");
        }
        (out.status.code().unwrap_or(-1), text)
    }

    fn run(&self) -> (i32, String) {
        self.run_rule(RULE)
    }
}

/// An RFC 3339 instant `minutes` from now, in the broker's own spelling
/// (chrono's `to_rfc3339`, `+00:00` and nanoseconds).
fn at(minutes: i64) -> String {
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("{minutes} minutes"),
            "+%Y-%m-%dT%H:%M:%S.123456789+00:00",
        ])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn last8(s: &str) -> &str {
    &s[s.len() - 8..]
}

#[test]
fn a_live_token_is_rendered_root_only_with_its_expiry_beside_it() {
    let c = Case::new("live");
    let exp = at(50);
    c.secret(&[("token", TOKEN), ("token.expires-at", &exp)]);
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("rendered: …{}", last8(TOKEN))),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&c.dest).unwrap(), TOKEN);
    assert_eq!(std::fs::read_to_string(c.expires_file()).unwrap(), exp);
    let mode = std::fs::metadata(&c.dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the token file is 0600");

    // The next tick with the same value touches nothing.
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(&format!("held: …{}", last8(TOKEN))), "{out}");

    // A re-mint reaches the file on the next tick.
    let exp2 = at(59);
    c.secret(&[("token", NEWER), ("token.expires-at", &exp2)]);
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(std::fs::read_to_string(&c.dest).unwrap(), NEWER);
    assert_eq!(std::fs::read_to_string(c.expires_file()).unwrap(), exp2);
}

#[test]
fn an_empty_secret_removes_the_file_so_the_push_refuses_its_empty_slot() {
    let c = Case::new("empty");
    c.held(TOKEN, &at(50));
    c.secret(&[]);
    let (rc, out) = c.run();
    assert_eq!(
        rc, 0,
        "the root not yet placed is not this pass's fault: {out}"
    );
    assert!(out.contains("empty:"), "{out}");
    assert!(!c.dest.exists() && !c.expires_file().exists(), "{out}");
}

#[test]
fn an_expired_token_is_never_rendered_and_the_pass_is_red() {
    let past = at(-5);
    let cases: [(&str, Vec<(&str, &str)>); 3] = [
        (
            "expired",
            vec![("token", TOKEN), ("token.expires-at", &past)],
        ),
        ("no expiry recorded", vec![("token", TOKEN)]),
        (
            "an expiry that is not a time",
            vec![("token", TOKEN), ("token.expires-at", "soon")],
        ),
    ];
    for (why, entries) in cases {
        let c = Case::new("expired");
        c.held(NEWER, &at(30));
        c.secret(&entries);
        let (rc, out) = c.run();
        assert_eq!(rc, 1, "{why}: the broker's refresh stopped — loud: {out}");
        assert!(out.contains("expired:"), "{why}: {out}");
        assert!(
            !c.dest.exists(),
            "{why}: no dead token left for the push to use, and no stale one either: {out}"
        );
    }
}

#[test]
fn an_unreachable_secret_keeps_a_live_held_token_and_removes_a_dead_one() {
    // No directory: the stub answers NotFound, as the API server does.
    let c = Case::new("absent-live");
    c.held(TOKEN, &at(40));
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("kept: absent"), "{out}");
    assert_eq!(std::fs::read_to_string(&c.dest).unwrap(), TOKEN);

    let c = Case::new("absent-dead");
    c.held(TOKEN, &at(-1));
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("removed: absent"), "{out}");
    assert!(!c.dest.exists());
}

#[test]
fn a_rule_for_any_other_handler_is_refused() {
    let c = Case::new("refused");
    c.secret(&[("token", TOKEN), ("token.expires-at", &at(50))]);
    let (rc, out) =
        c.run_rule("infra/dispatcher/rules/broker-rotates-the-forge-host-checkout-token.toml");
    assert_eq!(rc, 78, "{out}");
    assert!(
        out.contains("not a GitHub App installation-token rule"),
        "{out}"
    );
    assert!(!c.dest.exists());
}

fn epoch(instant: &str) -> i64 {
    let out = Command::new("date")
        .args(["-u", "-d", instant, "+%s"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{instant} is not a time");
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn now() -> i64 {
    epoch("now")
}

// ---------------------------------------------------------------------------
// --expire: the act that used the token is over (design 76c46869; backlog
// bf8726c9). github-act.sh revokes the per-act admin token after its write,
// and a revoked token must never read as live: the broker's approval mint
// re-mints only inside a 40-minute window, so a revoked token under its
// old expiry would be handed to the next act approved within ~20 minutes.
// The handler's own revoke sets the expiry to now first; so does this.
// ---------------------------------------------------------------------------

#[test]
fn expire_marks_the_token_the_act_used_expired_and_the_next_render_drops_it() {
    let c = Case::new("expire");
    let exp = at(50);
    c.secret(&[("token", TOKEN), ("token.expires-at", &exp)]);
    c.held(TOKEN, &exp);
    let before = now();
    let (rc, out) = c.run_expire(TOKEN);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("marked expired: …{}", last8(TOKEN))),
        "{out}"
    );
    assert!(!c.dest.exists() && !c.expires_file().exists(), "{out}");
    let marked = std::fs::read_to_string(c.secret_dir().join("token.expires-at")).unwrap();
    let at_ = epoch(&marked);
    assert!(
        (before - 1..=now() + 1).contains(&at_),
        "the Secret's expiry reads {marked}, not now: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(c.secret_dir().join("token")).unwrap(),
        TOKEN,
        "only the expiry is written; the value stays for the broker to replace"
    );
    let patch = std::fs::read_to_string(c.secrets.join("patches.log")).unwrap();
    assert!(
        patch.contains(r#""op":"test","path":"/metadata/resourceVersion""#),
        "the write is conditional on the object the read saw: {patch}"
    );

    // Every later reader sees "not live": the render drops the slot.
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("expired:") && !c.dest.exists(), "{out}");
}

#[test]
fn expire_leaves_a_newer_token_alone() {
    let c = Case::new("expire-newer");
    let exp = at(55);
    c.secret(&[("token", NEWER), ("token.expires-at", &exp)]);
    c.held(TOKEN, &at(30));
    let (rc, out) = c.run_expire(TOKEN);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("left:"), "{out}");
    assert!(
        out.contains(last8(NEWER)) && out.contains(last8(TOKEN)),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(c.secret_dir().join("token.expires-at")).unwrap(),
        exp,
        "a token minted for another act since this one read its own keeps its expiry"
    );
    assert!(
        !c.secrets.join("patches.log").exists(),
        "nothing was written"
    );
    assert!(
        !c.dest.exists(),
        "the slot is dropped either way — the next act renders its own"
    );
}

#[test]
fn expire_that_loses_a_race_or_cannot_reach_the_secret_is_red() {
    let c = Case::new("expire-race");
    let exp = at(50);
    c.secret(&[("token", TOKEN), ("token.expires-at", &exp)]);
    std::fs::write(c.secrets.join("race"), "").unwrap();
    c.held(TOKEN, &exp);
    let (rc, out) = c.run_expire(TOKEN);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("not marked:"), "{out}");
    assert_eq!(
        std::fs::read_to_string(c.secret_dir().join("token.expires-at")).unwrap(),
        exp
    );
    assert!(!c.dest.exists(), "{out}");

    // No Secret at all: nothing holds the token, so nothing to mark.
    let c = Case::new("expire-absent");
    c.held(TOKEN, &at(50));
    let (rc, out) = c.run_expire(TOKEN);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("absent:") && !c.dest.exists(), "{out}");

    // The token file the act used must be one: an unreadable one refuses.
    let c = Case::new("expire-bad");
    let (rc, out) = c.run_args(
        RULE,
        &["--expire", &c.root.join("nothing").display().to_string()],
    );
    assert_eq!(rc, 78, "{out}");
}

// ---------------------------------------------------------------------------
// --request: the org-admin token is keyed per ops-request (backlog 4ce4ec55).
// One key per owner stranded the second of two approved writes: write 1
// marked the shared key expired and revoked its token, and write 2's plan
// re-run rendered that expired key and refused. The broker now mints into
// `token-<request id>`, so the render reads the request's own key, and
// --expire marks that key alone.
// ---------------------------------------------------------------------------

const ORG_RULE: &str = "infra/dispatcher/rules/broker-mints-the-algedonic-dev-admin-token-when-a-github-request-is-filed.toml";
const R1: &str = "11111111-1111-4111-8111-111111111111";
const R2: &str = "22222222-2222-4222-8222-222222222222";

impl Case {
    /// A request's key as the broker's refresh writes it: the token, its
    /// expiry, and `minted-for` naming the request, in one write.
    fn minted(&self, request: &str, token: &str, expires: &str) {
        self.secret(&[
            (&format!("token-{request}"), token),
            (&format!("token-{request}.expires-at"), expires),
            (&format!("token-{request}.minted-for"), request),
        ]);
    }
}

#[test]
fn a_per_request_rule_renders_the_requests_own_key_and_no_other() {
    let c = Case::per_request("per-request");
    let (e1, e2) = (at(50), at(55));
    c.minted(R1, TOKEN, &e1);
    c.minted(R2, NEWER, &e2);
    let (rc, out) = c.run_args(ORG_RULE, &["--request", R2]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(std::fs::read_to_string(&c.dest).unwrap(), NEWER, "{out}");
    assert_eq!(std::fs::read_to_string(c.expires_file()).unwrap(), e2);
    assert_eq!(
        std::fs::read_to_string(c.request_file()).unwrap(),
        R2,
        "the slot says whose token it holds: {out}"
    );

    // A request the broker has not minted for yet reads as an empty slot,
    // never as its sibling's token.
    let c = Case::per_request("per-request-unminted");
    c.minted(R1, TOKEN, &e1);
    let (rc, out) = c.run_args(ORG_RULE, &["--request", R2]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("empty:"), "{out}");
    assert!(!c.dest.exists(), "{out}");
}

#[test]
fn a_per_request_rule_needs_its_request_and_a_standing_rule_takes_none() {
    let c = Case::per_request("per-request-missing");
    c.secret(&[("token", TOKEN), ("token.expires-at", &at(50))]);
    let (rc, out) = c.run_args(ORG_RULE, &[]);
    assert_eq!(
        rc, 78,
        "a per-request rule rendered without a request: {out}"
    );
    assert!(out.contains("--request"), "{out}");
    assert!(!c.dest.exists(), "{out}");

    for bad in [
        "../x",
        "11111111-1111-4111-8111-11111111111",
        "NOT-A-UUID",
        "-",
    ] {
        let (rc, out) = c.run_args(ORG_RULE, &["--request", bad]);
        assert_eq!(rc, 78, "request {bad:?}: {out}");
    }

    let c = Case::new("standing-with-request");
    c.secret(&[("token", TOKEN), ("token.expires-at", &at(50))]);
    let (rc, out) = c.run_args(RULE, &["--request", R1]);
    assert_eq!(
        rc, 78,
        "the DR token is one standing key; a request names nothing there: {out}"
    );
    assert!(!c.dest.exists(), "{out}");
}

#[test]
fn expire_marks_only_the_requests_own_key() {
    let c = Case::per_request("per-request-expire");
    let (e1, e2) = (at(50), at(55));
    c.minted(R1, TOKEN, &e1);
    c.minted(R2, NEWER, &e2);
    c.held_for(TOKEN, &e1, R1);
    let used = c.root.join("used.token");
    std::fs::write(&used, format!("{TOKEN}\n")).unwrap();
    let before = now();
    let (rc, out) = c.run_args(
        ORG_RULE,
        &["--request", R1, "--expire", &used.display().to_string()],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("marked expired: …{}", last8(TOKEN))),
        "{out}"
    );
    let marked =
        std::fs::read_to_string(c.secret_dir().join(format!("token-{R1}.expires-at"))).unwrap();
    assert!(
        (before - 1..=now() + 1).contains(&epoch(&marked)),
        "{marked}: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(c.secret_dir().join(format!("token-{R2}.expires-at"))).unwrap(),
        e2,
        "the other request's token was marked expired by this act: {out}"
    );

    assert!(
        !c.dest.exists() && !c.request_file().exists(),
        "the slot and the request it named outlived the act: {out}"
    );

    // And the other request's render still finds its own live token.
    let (rc, out) = c.run_args(ORG_RULE, &["--request", R2]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(std::fs::read_to_string(&c.dest).unwrap(), NEWER, "{out}");
}

// The review of c21401da, F1 (blocking). The slot file is per OWNER, and a
// plan keeps it, so the next request the serial runner answers often finds
// a sibling's live token already there. When the Secret could not be read,
// the render KEPT a held live token — so request 2 acted with request 1's
// token and then revoked it, stranding request 1 on the degraded path. A
// per-request slot now names the request it was rendered for
// (`<dest>.request`), and a failed read keeps it only for that request.

/// The API server unreachable, as the DNS stall class leaves it.
const TIMEOUT_KUBECTL: &str = "#!/bin/sh\n\
    echo 'Unable to connect to the server: dial tcp: i/o timeout' >&2\n\
    exit 1\n";

#[test]
fn a_failed_read_never_keeps_another_requests_token() {
    // The reviewer's repro-keep.sh: an API error, the slot holding request
    // 1's live token, rendered for request 2.
    let c = Case::per_request("keep-sibling-error");
    write_exec(&c.kubectl, TIMEOUT_KUBECTL);
    c.held_for(TOKEN, &at(50), R1);
    let (rc, out) = c.run_args(ORG_RULE, &["--request", R2]);
    assert_eq!(
        rc, 1,
        "an unreadable Secret is a fault this pass names: {out}"
    );
    assert!(
        !c.dest.exists() && !c.expires_file().exists() && !c.request_file().exists(),
        "request 2's render kept request 1's token: {out}"
    );

    // The Secret NotFound: the same.
    let c = Case::per_request("keep-sibling-absent");
    c.held_for(TOKEN, &at(50), R1);
    let (_, out) = c.run_args(ORG_RULE, &["--request", R2]);
    assert!(!c.dest.exists(), "{out}");

    // A slot that names no request is no request's.
    let c = Case::per_request("keep-unnamed");
    write_exec(&c.kubectl, TIMEOUT_KUBECTL);
    c.held(TOKEN, &at(50));
    let (_, out) = c.run_args(ORG_RULE, &["--request", R2]);
    assert!(!c.dest.exists(), "{out}");

    // The request's OWN live token rides out a transient failure, as
    // before: the plan re-run is not refused by a three-second stall.
    let c = Case::per_request("keep-own");
    write_exec(&c.kubectl, TIMEOUT_KUBECTL);
    c.held_for(TOKEN, &at(50), R2);
    let (rc, out) = c.run_args(ORG_RULE, &["--request", R2]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("kept:"), "{out}");
    assert_eq!(std::fs::read_to_string(&c.dest).unwrap(), TOKEN);
}

/// The render reads the key named after its request, and the key says
/// whom it was minted for: the two must agree, or the token is someone
/// else's however it got there.
#[test]
fn a_key_not_minted_for_its_request_is_never_rendered() {
    for (why, minted_for) in [("another request", Some(R1)), ("no request", None)] {
        let c = Case::per_request("minted-for-mismatch");
        c.held_for(TOKEN, &at(40), R2);
        c.secret(&[
            (&format!("token-{R2}"), NEWER),
            (&format!("token-{R2}.expires-at"), &at(55)),
        ]);
        if let Some(m) = minted_for {
            c.secret(&[(&format!("token-{R2}.minted-for"), m)]);
        }
        let (rc, out) = c.run_args(ORG_RULE, &["--request", R2]);
        assert_eq!(rc, 1, "{why}: {out}");
        assert!(out.contains("minted for"), "{why}: {out}");
        assert!(
            !c.dest.exists() && !c.request_file().exists(),
            "{why}: a token minted for someone else was left in the slot: {out}"
        );
    }
}

fn converge_code() -> Vec<String> {
    std::fs::read_to_string(repo_root().join(CONVERGE))
        .expect("forge-converge.sh")
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Render precedes push, on every tick: the render line is top-level (not
/// under a condition), comes before the off-site push, is handed the DR
/// token's broker rule, and its verdict decides the converge's exit.
#[test]
fn the_forge_converge_renders_the_token_before_every_push() {
    let code = converge_code();
    let at = |needle: &str| {
        code.iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("forge-converge.sh has no line running {needle:?}"))
    };
    let render = at("credential-render.sh");
    let push = at("infra/forge/offsite-push.sh");
    assert!(
        render < push,
        "the token is rendered BEFORE the push reads it, on the same tick"
    );
    assert!(
        !code[render].starts_with(' ') && !code[render].starts_with('\t'),
        "the render runs unconditionally, at the top level: {}",
        code[render]
    );
    assert!(
        code[render..push]
            .join("\n")
            .contains(RULE.trim_start_matches("infra/")),
        "the render is handed the DR token's broker rule, the one declaration of its Secret"
    );
    assert!(
        code.iter()
            .any(|l| l.contains("render_rc") && l.contains("exit")),
        "the render's verdict decides the converge's exit"
    );
}

/// The file the render writes is the file the push reads. offsite-push.json
/// names each GitHub target's `credential` and `token_file` (backlog
/// 761bc8a9, car 2 of 67931115); once a target declares the DR token, its
/// file and the render's destination are one fact in two files (§9a).
#[test]
fn the_render_writes_the_file_the_dr_target_reads() {
    let converge = std::fs::read_to_string(repo_root().join(CONVERGE)).unwrap();
    let marker = "BOSS_GITHUB_DR_TOKEN_FILE:-";
    let start = converge.find(marker).expect("the render's destination") + marker.len();
    let dest = &converge[start..start + converge[start..].find('}').unwrap()];
    assert_eq!(dest, "/etc/boss-publish/github-dr.token");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo_root().join(OFFSITE_JSON)).unwrap())
            .expect("offsite-push.json");
    fn targets<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
        match v {
            serde_json::Value::Object(m) => {
                if m.get("credential").and_then(|c| c.as_str()) == Some("github-dr-push-token") {
                    out.push(v);
                }
                m.values().for_each(|x| targets(x, out));
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| targets(x, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    targets(&json, &mut found);
    for t in found {
        assert_eq!(
            t.get("token_file").and_then(|f| f.as_str()),
            Some(dest),
            "offsite-push.json's DR target reads a different file than the render writes: {t}"
        );
    }
}
