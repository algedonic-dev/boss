//! infra/estate/node-roles.sh — the ONE definition of how a managed host
//! reads its roles off the system of record, sourced by boss-gcp's and
//! the forge's converge (design 1bc4b4ed; the second host is why it is
//! a file and not a second copy, CLAUDE.md §9a).
//!
//! Pinned here: `has_role` matches a whole role and never a substring
//! (`operator` must not match `cluster-operator`, or the credential
//! check would run on a host that never declared the role); a caller's
//! preset BOSS_NODE_ROLES wins over the registry read; and a registry
//! that does not answer leaves the roles EMPTY and says so, rather than
//! failing the converge — an arm that needs the patient is not an arm.
//! The two converges and install.sh are pinned to source the file, so
//! the definition cannot quietly become inline again.

use boss_testing::repo_root;
use std::process::Command;

fn sh(script: &str, env: &[(&str, &str)]) -> (i32, String) {
    let lib = repo_root().join("infra/estate/node-roles.sh");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(format!(". '{}'\n{script}", lib.display()))
        .env_remove("BOSS_NODE_ROLES");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn has_role_matches_a_whole_role_and_never_a_substring() {
    let (rc, _) = sh(
        "has_role cluster-operator",
        &[("BOSS_NODE_ROLES", "off-cluster-observer,cluster-operator")],
    );
    assert_eq!(rc, 0, "a declared role is found");
    let (rc, _) = sh(
        "has_role operator",
        &[("BOSS_NODE_ROLES", "off-cluster-observer,cluster-operator")],
    );
    assert_ne!(rc, 0, "`operator` is not a role this host declares");
    let (rc, _) = sh("has_role cluster-operator", &[]);
    assert_ne!(rc, 0, "no roles declared, nothing matches");
}

#[test]
fn a_preset_roles_list_wins_and_an_unreachable_registry_leaves_it_empty() {
    let (rc, out) = sh(
        "read_node_roles forge; echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("BOSS_NODE_ROLES", "cluster-operator"),
            ("BOSS_ESTATE_NODES_URL", "http://127.0.0.1:9/never"),
        ],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("roles=<cluster-operator> source=preset"),
        "a caller's preset list is not overwritten by a read: {out}"
    );

    // A dark registry with NO cache to fall back on: the read installs
    // [always] only — a sentinel no roles.toml section matches — never
    // every row. Widening what a host runs on a failed read was the
    // hazard 6cd124c4 filed the day boss-gcp stopped declaring the
    // legacy stack.
    let cache = boss_testing::scratch_dir("node-roles-nocache").join("roles.cache");
    let _ = std::fs::remove_file(&cache);
    let (rc, out) = sh(
        "read_node_roles forge; echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("BOSS_ESTATE_NODES_URL", "http://127.0.0.1:9/never"),
            ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
        ],
    );
    assert_eq!(rc, 0, "an unreachable registry is not a failed converge");
    assert!(
        out.contains("did not answer") && out.contains("no cached declaration"),
        "the read says why and what it did: {out}"
    );
    assert!(
        out.contains("roles=<registry-unread> source=none"),
        "a sentinel no role section matches, so only [always] installs — and the source says no read happened, \
         so a verb with a stricter policy (the retire verb refuses unless the registry answered) is not fooled \
         by a non-empty list: {out}"
    );
}

/// A successful read is REMEMBERED beside the checkout, and a dark
/// registry then installs the last declaration it has evidence for —
/// stamped as cached — rather than every row or nothing.
#[test]
fn a_dark_registry_installs_the_last_declaration_it_read() {
    let dir = boss_testing::scratch_dir("node-roles-cache");
    let cache = dir.join("roles.cache");
    let _ = std::fs::remove_file(&cache);
    // A stub registry: one node with two roles.
    let nodes = dir.join("nodes.json");
    std::fs::write(
        &nodes,
        r#"{"data":[{"id":"boss-gcp","roles":["ml-batch-host","off-cluster-observer"]}]}"#,
    )
    .unwrap();
    let url = format!("file://{}", nodes.display());
    let (rc, out) = sh(
        "read_node_roles boss-gcp; echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("BOSS_ESTATE_NODES_URL", &url),
            ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
        ],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("roles=<ml-batch-host,off-cluster-observer> source=registry"),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(&cache).unwrap().trim(),
        "ml-batch-host,off-cluster-observer",
        "the read is remembered"
    );
    // Now the registry is dark: the cache answers, and the log says so.
    let (rc, out) = sh(
        "read_node_roles boss-gcp; echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("BOSS_ESTATE_NODES_URL", "http://127.0.0.1:9/never"),
            ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
        ],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("roles=<ml-batch-host,off-cluster-observer> source=cache"),
        "a caller with its own policy can see the roles did NOT come from a live read: {out}"
    );
    assert!(
        out.contains("did not answer") && out.contains("cached declaration"),
        "the fallback is named as a fallback: {out}"
    );
}

/// A scratch dir holding a stub `curl` that plays the registry the way
/// real curl answers `-w '\n%{http_code}'`: a body (the forge's roles
/// on a 2xx, the refusal's text otherwise), then a newline and `code`.
/// It records its argv, one per line. Returns (dir, PATH with the stub
/// first, the argv file).
fn stub_registry(tag: &str, code: &str) -> (std::path::PathBuf, String, std::path::PathBuf) {
    let dir = boss_testing::scratch_dir(tag);
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let seen = dir.join("curl-args.txt");
    let _ = std::fs::remove_file(&seen);
    boss_testing::write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
             printf '%s\\n' \"$@\" > '{seen}'\n\
             case '{code}' in\n\
                 2??) printf '%s' '{{\"data\":[{{\"id\":\"forge\",\"roles\":[\"ops-runner\"]}}]}}' ;;\n\
                 *) printf '%s' 'reading the estate registry is refused: no active rule' ;;\n\
             esac\n\
             printf '\\n%s' '{code}'\n",
            seen = seen.display(),
        ),
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (dir, path, seen)
}

/// A REFUSED READ SAYS REFUSED (review of car 4e9e75e3). The registry
/// refuses a caller policy does not grant; `curl -f` turned that 403
/// into silence, and the read reported "did not answer" like a dark
/// system of record, then installed the cache. A lasting refusal — a
/// mis-signed header, a revoked audit-readonly grant — would have held
/// the host on its cached roles with nothing naming why. It still falls
/// back to the cache (a refusal never widens what a host runs), but the
/// log and the run's packet say REFUSED, with the code and the signer.
#[test]
fn a_refused_roles_read_says_refused_and_still_falls_back() {
    let (dir, path, _) = stub_registry("node-roles-refused", "403");
    let cache = dir.join("roles.cache");
    std::fs::write(&cache, "cluster-operator\n").unwrap();
    let absent = dir.join("absent.env");
    let env: &[(&str, &str)] = &[
        ("PATH", &path),
        (
            "BOSS_ESTATE_NODES_URL",
            "http://registry.test:7900/api/estate/nodes",
        ),
        ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
        ("BOSS_SOR_ENV", absent.to_str().unwrap()),
    ];
    let script = "run_summary_note() { echo \"NOTE: $*\"; }\n\
                  BOSS_CONVERGE_NAME=boss-gcp-converge read_node_roles forge; \
                  echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"";
    let (rc, out) = sh(script, env);
    assert_eq!(rc, 0, "a refusal is not a failed converge: {out}");
    assert!(
        out.contains("roles=<cluster-operator> source=cache"),
        "the cache still answers — a refusal never widens what a host runs: {out}"
    );
    let named = "REFUSED (HTTP 403, signed automation:boss-gcp-converge/audit-readonly)";
    assert!(
        out.lines()
            .any(|l| !l.starts_with("NOTE:") && l.contains(named)),
        "the log names the refusal, the code and the signer: {out}"
    );
    assert!(
        out.lines()
            .any(|l| l.starts_with("NOTE:") && l.contains(named)),
        "the run's packet names it too: {out}"
    );
    assert!(
        !out.contains("did not answer"),
        "a refusal is not a dark registry: {out}"
    );

    // With no cache, [always] only — and still named as a refusal.
    std::fs::remove_file(&cache).unwrap();
    let (rc, out) = sh(script, env);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("roles=<registry-unread> source=none") && out.contains(named),
        "{out}"
    );
}

/// ANY OTHER NON-2xx IS A DARK REGISTRY, NEVER A DECLARATION (review of
/// car 5957f14b). Dropping `curl -f` for the code meant every non-2xx
/// had to be handled by hand, and the arm that does it was unpinned:
/// without it a 503 (the policy service down — the estate reads ask it
/// now) or a 404 (the wrong port, CLAUDE.md §Doors) parsed its error
/// body as a registry with no node, installed EVERY row, and overwrote
/// the good cache with nothing. Each must install the cache, leave the
/// cache file as it was, and say which code it got.
#[test]
fn a_non_2xx_roles_read_keeps_the_cache_and_says_the_code() {
    for code in ["503", "404"] {
        let (dir, path, _) = stub_registry(&format!("node-roles-http-{code}"), code);
        let cache = dir.join("roles.cache");
        std::fs::write(&cache, "cluster-operator,ops-runner\n").unwrap();
        let absent = dir.join("absent.env");
        let (rc, out) = sh(
            "run_summary_note() { echo \"NOTE: $*\"; }\n\
             BOSS_CONVERGE_NAME=forge-converge read_node_roles forge; \
             echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
            &[
                ("PATH", &path),
                (
                    "BOSS_ESTATE_NODES_URL",
                    "http://registry.test:7900/api/estate/nodes",
                ),
                ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
                ("BOSS_SOR_ENV", absent.to_str().unwrap()),
            ],
        );
        assert_eq!(rc, 0, "HTTP {code}: {out}");
        assert!(
            out.contains("roles=<cluster-operator,ops-runner> source=cache"),
            "HTTP {code} installs the cached declaration, never every row: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&cache).unwrap(),
            "cluster-operator,ops-runner\n",
            "HTTP {code} must not overwrite the cache: {out}"
        );
        let said = format!("did not answer (HTTP {code})");
        assert!(
            out.lines()
                .any(|l| !l.starts_with("NOTE:") && l.contains(&said))
                && out
                    .lines()
                    .any(|l| l.starts_with("NOTE:") && l.contains(&said)),
            "HTTP {code}: the log and the run's packet name the code: {out}"
        );
        assert!(
            !out.contains("installing every row") && !out.contains("REFUSED"),
            "HTTP {code} is neither a declaration nor a refusal: {out}"
        );
    }
}

/// THE ROLES READ IS SIGNED, AS A READER (backlog e5f7b51e). It was a
/// bare `curl` — the one in-tree reader of `/api/estate/nodes` that
/// named nobody — so the registry could not refuse an anonymous caller
/// without every host converge losing its live roles to the cache. It
/// now carries `x-boss-user` from `sor_reader_header` (infra/lib/sor.sh):
/// the platform's read role at the auditor tier, named after the
/// converge that reads, so the read is attributable and cannot write.
#[test]
fn the_roles_read_is_signed_as_a_named_reader() {
    let (dir, path, seen) = stub_registry("node-roles-signed", "200");
    let cache = dir.join("roles.cache");
    let (rc, out) = sh(
        "BOSS_CONVERGE_NAME=forge-converge read_node_roles forge; \
         echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("PATH", &path),
            (
                "BOSS_ESTATE_NODES_URL",
                "http://registry.test:7900/api/estate/nodes",
            ),
            ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
            // No host file leaks into the read under test.
            ("BOSS_SOR_ENV", dir.join("absent.env").to_str().unwrap()),
        ],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("roles=<ops-runner> source=registry"),
        "the stub answered, so the read is the registry's: {out}"
    );
    let args: Vec<String> = std::fs::read_to_string(&seen)
        .expect("the read reached curl")
        .lines()
        .map(str::to_string)
        .collect();
    let user = args
        .windows(2)
        .find(|w| w[0] == "-H" && w[1].starts_with("x-boss-user: "))
        .map(|w| w[1].trim_start_matches("x-boss-user: ").to_string())
        .unwrap_or_else(|| {
            panic!(
                "GET /api/estate/nodes went out UNSIGNED — the registry refuses a caller with \
                 no name, and a refused read installs the cache as if the registry were dark: \
                 {args:?}"
            )
        });
    let v: serde_json::Value = serde_json::from_str(&user)
        .unwrap_or_else(|e| panic!("x-boss-user is not JSON ({e}): {user}"));
    assert_eq!(
        v["role"].as_str(),
        Some(boss_core::roles::AUDIT_READONLY_ROLE),
        "a converge reads as the platform's READ role, never one that can write: {user}"
    );
    assert_eq!(v["access_tier"].as_str(), Some("auditor"), "{user}");
    assert_eq!(
        v["id"].as_str(),
        Some("automation:forge-converge"),
        "the read names the converge that made it: {user}"
    );
}

#[test]
fn every_converge_and_the_installer_source_the_one_definition() {
    for f in [
        "infra/gcp/boss-gcp-converge.sh",
        "infra/forge/forge-converge.sh",
        "infra/forge/install.sh",
    ] {
        let text = std::fs::read_to_string(repo_root().join(f)).expect(f);
        assert!(
            text.contains("estate/node-roles.sh"),
            "{f} must source infra/estate/node-roles.sh, not carry its own role read"
        );
    }
    let unit = std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.service"))
        .expect("forge-converge.service");
    assert!(
        unit.contains("Environment=BOSS_NODE_ID=forge"),
        "the forge's node id is declared on its unit, never guessed from a hostname"
    );
}
