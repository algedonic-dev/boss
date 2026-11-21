//! The root verbs resolve a path where the KERNEL reaches it, never
//! through `realpath -e` / `realpath -m` (backlog 9b74c84b).
//!
//! WHY. restore-shared leaned on GNU `realpath -e` and failed on
//! GitHub's runner while the cluster gate stayed green (920f4fd5): a
//! realpath that collapses `..` as TEXT answers `a/link/../f` with
//! `a/f`, while the kernel follows `link` first and reaches the `f`
//! beside the link's target. The survey that car filed found the same
//! dependence in the root-run gcp verbs and the agent image publisher,
//! where the resolved path is a refusal's input — a directory a backup
//! removal may touch, a token's parent, a generation's entrypoint. Which
//! coreutils a host ships is not a root verb's to depend on, so each
//! resolves one component at a time, asking the kernel (`[ -L ]`,
//! `readlink`) about a prefix already resolved physically — the walk
//! GNU's realpath makes, which is why a differential against it below is
//! the control (review 5e3eb6f3 found the first text, built on `cd -P`,
//! answered a link AFTER a cancelled missing prefix unresolved).
//!
//! ONE TEXT, MANY COPIES (CLAUDE.md §9a). The function cannot live in a
//! sourced library: these scripts validate the very tree a library
//! would be read from (the bootstrap closure is an explicit file list in
//! prepare-root-tree.sh, and adopt-root-tree.sh runs before any
//! bootstrap exists), which is why `safe()` is already reproduced in
//! each. So it is copied, and this file holds every copy to one text
//! and runs that text against the shapes that tell the kernel from a
//! textual resolver.

use boss_testing::{create_dir, repo_root, scratch_dir, write_file};
use std::path::Path;
use std::process::Command;

/// Every script that resolves a path for a refusal, and so carries a
/// copy of `kernel_path`.
const CARRIERS: &[&str] = &[
    "infra/gcp/reclaim-gcp-root.sh",
    "infra/gcp/root-tree.sh",
    "infra/gcp/root-tree-credential.sh",
    "infra/gcp/gcp-root-launch.sh",
    "infra/gcp/root-tree-status.sh",
    "infra/gcp/adopt-root-tree.sh",
    "infra/gcp/prepare-root-tree.sh",
    "infra/agent/publish-image.sh",
];

/// The `kernel_path() {` … `}` block of `rel`, verbatim.
fn kernel_path_of(rel: &str) -> String {
    let path = repo_root().join(rel);
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = String::new();
    let mut inside = false;
    for line in body.lines() {
        if line == "kernel_path() {" {
            inside = true;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
            if line == "}" {
                return out;
            }
        }
    }
    panic!(
        "{rel} defines no `kernel_path() {{` … `}}` block — a root verb that \
         resolves a path for a refusal does it through the kernel (backlog 9b74c84b)"
    )
}

/// `kernel_path <mode> <path>` as the copy in `rel` answers it:
/// `Some(stdout)` on success, `None` when it refuses.
fn resolve(rel: &str, mode: &str, path: &Path) -> Option<String> {
    let script = format!(
        "set -euo pipefail\n{}kernel_path \"$1\" \"$2\"\n",
        kernel_path_of(rel)
    );
    let o = Command::new("bash")
        .arg("-c")
        .arg(script)
        .arg("kernel-path")
        .arg(mode)
        .arg(path)
        .env("CDPATH", "/")
        .output()
        .unwrap_or_else(|e| panic!("spawn bash: {e}"));
    o.status.success().then(|| {
        String::from_utf8_lossy(&o.stdout)
            .trim_end_matches('\n')
            .to_owned()
    })
}

/// The fixture: `top/x/y` is a real directory, `link` points at it, and
/// `top/x/f` and a decoy `f` both exist — so `link/../f` names
/// `top/x/f` to the kernel and the decoy to a resolver that drops
/// `link/..` as text. The root is resolved physically first, because
/// the temp dir itself may sit behind a link.
fn fixture() -> std::path::PathBuf {
    let raw = scratch_dir("kernel-path");
    let dir = std::fs::canonicalize(&raw)
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", raw.display()));
    create_dir(&dir.join("top/x/y"));
    write_file(&dir.join("top/x/f"), "reached through the link\n");
    write_file(&dir.join("f"), "the decoy a textual `..` reaches\n");
    std::os::unix::fs::symlink("top/x/y", dir.join("link")).expect("symlink link");
    std::os::unix::fs::symlink("top/x/f", dir.join("file-link")).expect("symlink file-link");
    std::os::unix::fs::symlink("top/x/gone", dir.join("dangling")).expect("symlink dangling");
    dir
}

#[test]
fn every_copy_of_kernel_path_is_one_text() {
    let first = kernel_path_of(CARRIERS[0]);
    for rel in &CARRIERS[1..] {
        assert_eq!(
            kernel_path_of(rel),
            first,
            "{rel}'s kernel_path differs from {}'s — the copies are one text, \
             held equal here because a sourced library cannot serve a script \
             that validates the tree the library would be read from",
            CARRIERS[0]
        );
    }
}

#[test]
fn no_carrier_calls_realpath_e_or_m() {
    for rel in CARRIERS {
        let path = repo_root().join(rel);
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (n, line) in body.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with('#') {
                continue;
            }
            assert!(
                !code.contains("realpath -e") && !code.contains("realpath -m"),
                "{rel}:{} still resolves through `realpath`: `{}` — GNU's -e/-m is \
                 not what every host ships, and one that collapses `..` as text \
                 answers for a path no write reaches (backlog 9b74c84b)",
                n + 1,
                line.trim()
            );
        }
    }
}

/// The shape 920f4fd5 measured: `..` after a symlink. The kernel follows
/// the link, then climbs from its TARGET.
#[test]
fn dot_dot_through_a_symlink_resolves_as_the_kernel_does() {
    let dir = fixture();
    let through = dir.join("link/../f");
    let kernel = dir.join("top/x/f").display().to_string();
    for rel in CARRIERS {
        for mode in ["e", "m"] {
            assert_eq!(
                resolve(rel, mode, &through).as_deref(),
                Some(kernel.as_str()),
                "{rel}: kernel_path {mode} {} must answer the file the kernel \
                 opens, not the decoy {}/f a textual `..` reaches",
                through.display(),
                dir.display()
            );
        }
        assert_eq!(
            resolve(rel, "e", &dir.join("link/..")).as_deref(),
            Some(dir.join("top/x").display().to_string().as_str()),
            "{rel}: a directory reached by `link/..` is the link target's parent"
        );
    }
}

/// `e` refuses a missing component as `realpath -e` did, so every site
/// keeps its refusal on a missing path; `m` answers the existing prefix
/// from the kernel and the missing rest as written.
#[test]
fn a_missing_path_is_refused_by_e_and_spelled_by_m() {
    let dir = fixture();
    let rel = CARRIERS[0];
    for missing in ["link/../gone", "top/gone/f", "dangling", "top/x/f/"] {
        assert_eq!(
            resolve(rel, "e", &dir.join(missing)),
            None,
            "kernel_path e {missing} must refuse: a component does not exist"
        );
    }
    let x = dir.join("top/x").display().to_string();
    for (input, want) in [
        ("link/../gone", format!("{x}/gone")),
        ("link/../gone/deeper", format!("{x}/gone/deeper")),
        ("dangling", format!("{x}/gone")),
        ("link/gone/..", format!("{x}/y")),
        ("link/gone/./z", format!("{x}/y/gone/z")),
    ] {
        assert_eq!(
            resolve(rel, "m", &dir.join(input)).as_deref(),
            Some(want.as_str()),
            "kernel_path m {input}"
        );
    }
}

/// The reviewer's fixture (review run 5e3eb6f3 on d718cd0e) and the
/// shapes around it: `live-link -> backup/running`, a chain onto it, a
/// link whose target climbs with `..`, an absolute link, a dangling
/// chain, loops. No `missing` exists, so `missing/..` cancels a prefix
/// the kernel cannot walk — the shape a component-at-a-time resolver must
/// still follow the links AFTER, because GNU `realpath -m` does, and the
/// reclaim's backup-reference guards compare that answer with each
/// candidate backup.
fn linked_fixture() -> std::path::PathBuf {
    let raw = scratch_dir("kernel-path-gnu");
    let dir = std::fs::canonicalize(&raw)
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", raw.display()));
    create_dir(&dir.join("backup/running/sub"));
    create_dir(&dir.join("top/x/y"));
    write_file(&dir.join("backup/running/bin"), "a binary in a backup\n");
    write_file(&dir.join("top/x/f"), "a file\n");
    write_file(&dir.join("running"), "the decoy a lost link reaches\n");
    let link = |target: &str, name: &str| {
        std::os::unix::fs::symlink(target, dir.join(name))
            .unwrap_or_else(|e| panic!("symlink {name} -> {target}: {e}"))
    };
    link("backup/running", "live-link");
    link("live-link", "chain2");
    link("chain2", "chain1");
    link("../top/x", "backup/up");
    link("backup/up/../y", "via-up");
    link(&dir.join("backup").display().to_string(), "abs-link");
    link("top/x/f", "file-link");
    link("top/x/gone", "dangling");
    link("dangling", "dangle-chain");
    link("loop", "loop");
    link("self-ref/x", "self-ref");
    dir
}

/// `realpath -m`/`-e` as GNU coreutils answers it, run in `cwd`.
fn gnu(mode: &str, path: &str, cwd: &Path) -> Option<String> {
    let o = Command::new("realpath")
        .arg(format!("-{mode}"))
        .arg("--")
        .arg(path)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("spawn realpath: {e}"));
    o.status.success().then(|| {
        String::from_utf8_lossy(&o.stdout)
            .trim_end_matches('\n')
            .to_owned()
    })
}

/// `kernel_path <mode> <path>` from the copy in `rel`, run in `cwd`.
fn resolve_in(rel: &str, mode: &str, path: &str, cwd: &Path) -> Option<String> {
    let script = format!(
        "set -euo pipefail\n{}kernel_path \"$1\" \"$2\"\n",
        kernel_path_of(rel)
    );
    let o = Command::new("bash")
        .arg("-c")
        .arg(script)
        .arg("kernel-path")
        .arg(mode)
        .arg(path)
        .current_dir(cwd)
        .env("CDPATH", "/")
        .output()
        .unwrap_or_else(|e| panic!("spawn bash: {e}"));
    o.status.success().then(|| {
        String::from_utf8_lossy(&o.stdout)
            .trim_end_matches('\n')
            .to_owned()
    })
}

/// Differential: every copy answers what GNU `realpath -e` / `-m` answers
/// on the same fixture, absolute and relative, for every shape the
/// review named and the ones beside it. GNU is the pre-change semantic
/// control — the car replaced it because a host may not ship it, not
/// because its answers were wrong — so this needs GNU's realpath and
/// says so rather than passing without it. A loop is the one shape
/// held apart: there `kernel_path` must refuse (no answer can be
/// established), whatever GNU spells.
#[test]
fn every_copy_answers_what_gnu_realpath_answers() {
    let version = Command::new("realpath")
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    assert!(
        version.contains("GNU coreutils"),
        "this differential needs GNU realpath as its control, and `realpath --version` \
         answered {version:?} — the mirror pins a GNU image (ci.yml) for this reason"
    );
    let dir = linked_fixture();
    let shapes = [
        // The review's two measured shapes (5e3eb6f3).
        "missing/../live-link",
        "missing/../live-link/../running",
        // Cancelled missing prefixes, then links, nested and chained.
        "missing/./../live-link",
        "missing/deeper/../../live-link/bin",
        "missing/../missing2/../chain1/sub",
        "missing/../chain1/../running/bin",
        "missing/../via-up/../f",
        "missing/../abs-link/running/bin",
        "missing/../dangle-chain/deeper",
        "live-link/missing/../..",
        "live-link/missing/../sub/../bin",
        "top/x/f/../../../chain1",
        // Links and `..` through existing paths.
        "live-link/../running",
        "chain1/../running/bin",
        "via-up",
        "backup/up/../y",
        "abs-link/up/f",
        // Trailing and doubled slashes.
        "live-link/",
        "chain1//",
        "missing/../chain1/",
        "file-link/",
        "file-link/..",
        "top/x/f/",
        "top/x/f/..",
        "dangling/",
        "dangle-chain",
        "missing//deeper/",
        "missing/",
        "missing",
        ".",
        "./",
    ];
    for rel in CARRIERS {
        for shape in shapes {
            let absolute = format!("{}/{shape}", dir.display());
            for input in [absolute.as_str(), shape] {
                for mode in ["e", "m"] {
                    assert_eq!(
                        resolve_in(rel, mode, input, &dir),
                        gnu(mode, input, &dir),
                        "{rel}: kernel_path {mode} {input} differs from GNU realpath -{mode} \
                         (cwd {})",
                        dir.display()
                    );
                }
            }
        }
        for looped in ["loop", "loop/x", "self-ref", "missing/../self-ref/y"] {
            let absolute = format!("{}/{looped}", dir.display());
            for mode in ["e", "m"] {
                assert_eq!(
                    resolve_in(rel, mode, &absolute, &dir),
                    None,
                    "{rel}: kernel_path {mode} {absolute} must refuse a symlink loop — \
                     no answer can be established, and a spelling would pass a guard"
                );
            }
        }
    }
}

/// A final symlink is followed, as `realpath` followed it, so a site
/// comparing the answer with its input still refuses a linked leaf.
#[test]
fn a_final_symlink_is_followed() {
    let dir = fixture();
    let rel = CARRIERS[0];
    assert_eq!(
        resolve(rel, "e", &dir.join("file-link")).as_deref(),
        Some(dir.join("top/x/f").display().to_string().as_str())
    );
    assert_eq!(
        resolve(rel, "e", &dir.join("link")).as_deref(),
        Some(dir.join("top/x/y").display().to_string().as_str())
    );
    let canonical = dir.join("top/x/f").display().to_string();
    assert_eq!(
        resolve(rel, "e", &dir.join("top/x/f")).as_deref(),
        Some(canonical.as_str()),
        "an existing path spelled physically answers itself"
    );
}
