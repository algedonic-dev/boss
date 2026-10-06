//! Every in-tree reader of the estate reads SIGNS its request (backlog
//! e5f7b51e, 2026-09-28).
//!
//! WHY THE ORDER MATTERS. `GET /api/estate/nodes`, `/observations` and
//! `/comparisons` answered a caller with no identity — every host's LAN
//! address, roles, capacity and disk readings — and were the last three
//! rows of the jobs API's PENDING ratchet
//! (`every_jobs_read_asks_policy_or_is_named_public.rs`). They could not
//! simply start refusing: a reader that sends no `x-boss-user` is then
//! refused, and a refused read in this estate does not error — it
//! answers a SMALLER WORLD. `infra/estate/node-roles.sh` read the
//! registry with a bare `curl` on every host converge, and a refusal
//! there installs the cached roles (or `[always]` only) and says the
//! registry "did not answer": the wrong-target-answers-instead-of-
//! erroring class of CLAUDE.md §Doors. So the car that made the reads
//! ask policy first signed every reader, and this pin holds that — the
//! reads may refuse because nothing in the tree reads them unsigned.
//!
//! THE READER LIST IS DERIVED, NOT TYPED (CLAUDE.md §9a). Every file
//! under `infra/`, `crates/`, `apps/`, `libs/` and `examples/` whose
//! CODE names one of the three paths is found by walking the tree —
//! comment lines, `#[cfg(test)]` modules, `tests/` directories and web
//! test files are not code that reads the record. Each found file must
//! sit on exactly one of two tables, and each row must still find its
//! file:
//!
//! - [`READERS`]: it reads the record, and the row names the door that
//!   signs the read plus a line of the tree proving that door carries
//!   `x-boss-user` — checked here, so a door that stops signing fails
//!   by name. `node-roles.sh`, the one reader that did not sign, is
//!   ALSO run against a stub `curl` in `node_roles_sh.rs`
//!   (`the_roles_read_is_signed_as_a_named_reader`).
//! - [`NOT_READERS`]: it names a path without reading it (a stub that
//!   PLAYS the registry), with the reason.
//!
//! A new reader of the estate therefore fails this test until a row
//! says how it signs.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The three reads that ask policy since this car.
const READS: &[&str] = &[
    "/api/estate/nodes",
    "/api/estate/observations",
    "/api/estate/comparisons",
];

/// The trees a reader could live in.
const TREES: &[&str] = &["infra", "crates", "apps", "libs", "examples"];

/// One reader: the file, the door it reads through, and the evidence —
/// a file and a line in it that shows the door sends `x-boss-user`.
struct Reader {
    file: &'static str,
    door: &'static str,
    proof_file: &'static str,
    proof: &'static str,
}

const READERS: &[Reader] = &[
    Reader {
        file: "infra/estate/node-roles.sh",
        door: "its own curl, signed by sor_reader_header (infra/lib/sor.sh) as \
               automation:<converge> at the read role — run against a stub curl in \
               node_roles_sh.rs",
        proof_file: "infra/estate/node-roles.sh",
        proof: "-H \"x-boss-user: $(sor_reader_header \"automation:$prefix\")\"",
    },
    Reader {
        file: "infra/forge/cluster-node-lib.sh",
        door: "its own curl, signed by sor_reader_header (infra/lib/sor.sh) as \
               automation:<verb> at the read role — the node verbs cordon-node, node-status, \
               shutdown-node and talos-get (backlog f0aaa72f), run against a file:// \
               registry in node_maintenance_verbs_sh.rs",
        proof_file: "infra/forge/cluster-node-lib.sh",
        proof: "-H \"x-boss-user: $(sor_reader_header \"automation:$ME\")\" \\\n\
                -w '\\n%{http_code}' \"$url\"",
    },
    Reader {
        file: "infra/ops/retire-ops-runner.sh",
        door: "its own curl for the other runners, signed by sor_reader_header \
               (infra/lib/sor.sh) as automation:retire-ops-runner at the read role, beside \
               node-roles.sh's read_node_roles for its own host — run against a stub curl \
               that records the signer in retire_ops_runner_sh.rs (backlog 98eb9349)",
        proof_file: "infra/ops/retire-ops-runner.sh",
        proof: "-H \"x-boss-user: $(sor_reader_header \"automation:$ME\")\" \\\n\
                \"${BOSS_ESTATE_NODES_URL:-$BASE/api/estate/nodes}\"",
    },
    Reader {
        file: "infra/estate/observe-units.sh",
        door: "hands the URL to node-roles.sh's read_node_roles in a bash child; it \
               never curls the registry itself",
        proof_file: "infra/estate/observe-units.sh",
        proof: "read_node_roles \"$2\"",
    },
    Reader {
        file: "infra/cluster/manifests/boss-estate-observe.yaml",
        door: "api_curl with OBSERVER_USER (automation:estate-observer, platform-admin)",
        proof_file: "infra/cluster/manifests/boss-estate-observe.yaml",
        proof: "api_curl -sf -H \"x-boss-user: $OBSERVER_USER\" \\\n\
                \"$JOBS_API/api/estate/observations?scope=kubernetes-evictions",
    },
    Reader {
        file: "crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_alarm.rs",
        door: "common::get_json, which signs as the rule (dispatcher_actor_header)",
        proof_file: "crates/orchestrators/boss-dispatcher-handlers/src/handlers/common.rs",
        proof: ".header(\"x-boss-user\", dispatcher_actor_header(rule_name))",
    },
    Reader {
        file: "crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_recover.rs",
        door: "common::get_json, which signs as the rule (dispatcher_actor_header)",
        proof_file: "crates/orchestrators/boss-dispatcher-handlers/src/handlers/common.rs",
        proof: ".header(\"x-boss-user\", dispatcher_actor_header(rule_name))",
    },
    Reader {
        file: "crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_compare.rs",
        door: "common::get_json, which signs as the rule (dispatcher_actor_header)",
        proof_file: "crates/orchestrators/boss-dispatcher-handlers/src/handlers/common.rs",
        proof: ".header(\"x-boss-user\", dispatcher_actor_header(rule_name))",
    },
    Reader {
        file: "crates/orchestrators/boss-cli/src/door.rs",
        door: "its READ is sent by `boss orient` through gate::api, which signs every \
               call (identity::signature_for — the reader role when no actor is named)",
        proof_file: "crates/orchestrators/boss-cli/src/orient.rs",
        proof: "crate::gate::api(&http, reqwest::Method::GET, crate::door::READ, None)",
    },
    Reader {
        file: "crates/orchestrators/boss-cli/src/train/conductor.rs",
        door: "the conductor's own api(), signed as automation:train-conductor",
        proof_file: "crates/orchestrators/boss-cli/src/train/conductor.rs",
        proof: ".header(\"x-boss-user\", boss_user());",
    },
    Reader {
        file: "apps/web/src/it/estate/estate.ts",
        door: "the browser, through the gateway's /api/estate proxy, which sets \
               x-boss-user from the session cookie (role_headers.rs) — David's \
               platform-admin session among them",
        proof_file: "crates/core/boss-gateway/src/main.rs",
        proof: "\"/api/estate/{*rest}\",",
    },
];

/// Whether `text` carries `proof`: each of its `\n`-separated fragments
/// on consecutive lines, in order — so a proof can tie the header to
/// the one call that reads, not to any call in the file.
fn evidenced(text: &str, proof: &str) -> bool {
    let want: Vec<&str> = proof.split('\n').collect();
    let lines: Vec<&str> = text.lines().collect();
    lines.windows(want.len()).any(|w| {
        w.iter()
            .zip(&want)
            .all(|(line, fragment)| line.contains(fragment))
    })
}

/// Files that name an estate read in code without reading the record.
const NOT_READERS: &[(&str, &str)] = &[
    (
        "crates/core/boss-jobs/src/http/mod.rs",
        "the server: it mounts the three routes, whose handlers ask policy \
         (every_jobs_read_asks_policy_or_is_named_public holds them to it)",
    ),
    (
        "infra/lint/an-eviction-is-recorded-where-the-record-can-read-it.sh",
        "a stub curl's case arm: the lint PLAYS the registry to the observer it tests",
    ),
];

/// Whether `line` names one of [`READS`] — `/api/estate/nodes/batch`,
/// the declaration WRITE, is not the nodes read.
fn names_a_read(line: &str) -> bool {
    READS.iter().any(|read| {
        line.match_indices(read)
            .any(|(i, _)| !line[i + read.len()..].starts_with('/'))
    })
}

/// Whether `line` is a comment in `path`'s language. By extension, so
/// that a shell `case` arm opening `*/api/…)` or a curl flag line
/// opening `--` is still read as the code it is.
fn is_comment(path: &Path, line: &str) -> bool {
    let t = line.trim_start();
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs" | "ts" | "js" | "svelte") => {
            t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')
        }
        Some("sql") => t.starts_with("--"),
        _ => t.starts_with('#'),
    }
}

/// The lines of `text` that are code: comment lines are dropped, and so
/// is every top-level `#[cfg(test)] mod … { … }` of a Rust file — read
/// by rustfmt's shape (the module opens at column 0 and closes at the
/// first line that is exactly `}`), since production code may follow
/// a test module.
fn code_lines(path: &Path, text: &str) -> Vec<String> {
    let rust = path.extension().is_some_and(|e| e == "rs");
    let mut out = Vec::new();
    let mut in_test_mod = false;
    let mut after_cfg_test = false;
    for line in text.lines() {
        if in_test_mod {
            in_test_mod = line != "}";
            continue;
        }
        if rust && line == "#[cfg(test)]" {
            after_cfg_test = true;
            continue;
        }
        if after_cfg_test {
            after_cfg_test = false;
            if line.starts_with("mod ") && line.ends_with('{') {
                in_test_mod = true;
                continue;
            }
        }
        if !is_comment(path, line) {
            out.push(line.to_string());
        }
    }
    out
}

/// Paths no reader lives under: build output, installed packages, and
/// test code, which plays the registry rather than reading it.
fn skipped(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("target" | "node_modules" | "tests" | "dist" | ".git")
        )
    }) || name.ends_with(".test.ts")
        || name.ends_with(".spec.ts")
        || name.ends_with(".md")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if skipped(&path) {
            continue;
        }
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Every file whose code names an estate read, relative to the root.
fn derived_readers() -> BTreeSet<String> {
    let root = repo_root();
    let mut files = Vec::new();
    for tree in TREES {
        walk(&root.join(tree), &mut files);
    }
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            code_lines(&path, &text)
                .iter()
                .any(|l| names_a_read(l))
                .then(|| {
                    path.strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned()
                })
        })
        .collect()
}

/// ONE READER SHAPE (CLAUDE.md §9a; review of car 4e9e75e3). The shell
/// spelling lives once, in infra/lib/sor-reader.sh — node-roles.sh (via
/// sor.sh) and every lint (via infra/lint/lib/sor-read.sh) source it —
/// but the Rust one, `boss_core::roles::reader_header`, which signs a
/// recorded probe and an unnamed CLI read and which every machine gate
/// stamps on a probe-reader match (design b35c22b4), cannot source a
/// shell file. So the two are held equal here, by value: the Rust one is
/// CALLED, not read out of its source, since it moved to a crate this
/// one depends on.
#[test]
fn a_read_of_the_record_signs_one_shape() {
    let root = repo_root();
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(". infra/lib/sor-reader.sh && sor_reader_header automation:pin")
        .current_dir(&root)
        .output()
        .expect("bash runs");
    let shell: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("sor_reader_header printed no JSON ({e}): {out:?}"));
    assert_eq!(shell["id"], "automation:pin", "{shell}");

    let rust: serde_json::Value =
        serde_json::from_str(&boss_core::roles::reader_header("automation:pin"))
            .expect("reader_header is JSON");
    assert!(
        rust.as_object().is_some_and(|o| o.len() >= 6),
        "too few fields in reader_header: {rust}"
    );
    assert_eq!(
        shell, rust,
        "infra/lib/sor-reader.sh and boss_core::roles::reader_header sign different shapes — \
         a machine read and a probe read must carry the same identity"
    );
}

#[test]
fn the_census_finds_the_readers_it_knows() {
    let found = derived_readers();
    // Controls whose answer is known: one shell reader, one Rust reader,
    // and the write that shares a prefix with a read and must NOT count.
    for known in [
        "infra/estate/node-roles.sh",
        "crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_alarm.rs",
    ] {
        assert!(found.contains(known), "lost {known}: {found:#?}");
    }
    assert!(
        !found.contains("crates/orchestrators/boss-cli/src/estate.rs"),
        "estate.rs POSTs /api/estate/nodes/batch — a write, not the read: {found:#?}"
    );
    assert!(names_a_read("x \"/api/estate/nodes\" y"));
    assert!(!names_a_read("x /api/estate/nodes/batch y"));
    assert!(!names_a_read("x /api/estate/observation\" y"));
}

#[test]
fn every_estate_read_is_signed() {
    let found = derived_readers();
    let root = repo_root();
    let mut failures = Vec::new();
    let listed: BTreeSet<&str> = READERS
        .iter()
        .map(|r| r.file)
        .chain(NOT_READERS.iter().map(|(f, _)| *f))
        .collect();
    for file in &found {
        if !listed.contains(file.as_str()) {
            failures.push(format!(
                "{file}: reads {READS:?} and no row says how it signs — the reads refuse a \
                 caller with no x-boss-user, and a refused read answers a smaller world. Sign \
                 it (infra/lib/sor.sh sor_reader_header for a script) and add a READERS row, \
                 or a NOT_READERS row if it only plays the registry"
            ));
        }
    }
    for file in &listed {
        if !found.contains(*file) {
            failures.push(format!(
                "{file}: listed, but its code no longer names an estate read — delete its row"
            ));
        }
    }
    for r in READERS {
        let text = std::fs::read_to_string(root.join(r.proof_file)).unwrap_or_default();
        if !evidenced(&text, r.proof) {
            failures.push(format!(
                "{}: its door ({}) is no longer evidenced — {} carries no line `{}`",
                r.file, r.door, r.proof_file, r.proof
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} estate reader(s) break the rule:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
