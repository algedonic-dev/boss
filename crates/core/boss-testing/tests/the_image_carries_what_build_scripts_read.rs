//! `infra/lint/the-image-carries-what-build-scripts-read.sh` is RUN
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (2026-09-19, train #473). boss-core gained
//! `include_str!("../../../../infra/platform/tiers.toml")` (ba429e7f);
//! the rust-build stage of `infra/oss-quickstart/Dockerfile` never
//! copied that file, so every image build from be78c815 died on
//! "couldn't read ../infra/platform/tiers.toml" and the cluster could
//! not converge. The lint that exists for exactly this said CARRIED,
//! because the RUNTIME stage has `COPY infra/platform /opt/boss/...`
//! and the ancestor probe matched it. A COPY in another stage is a
//! different filesystem. This file owns the verdict: only the COPYs of
//! the stage that runs cargo count, a runtime-stage COPY is not
//! coverage, an ancestor COPY in the cargo stage still is, and a
//! Dockerfile whose cargo stage cannot be found is red rather than
//! quietly clean.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/the-image-carries-what-build-scripts-read.sh";

/// A synthetic repository: one crate that `include_str!`s a file two
/// directories up (`infra/x/y.toml`), the file itself, the lint at the
/// path its own `cd "$(dirname $0)/../.."` resolves from, its helpers,
/// and a Dockerfile the test writes per case.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str, dockerfile: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("image-carries-{tag}"));
        boss_testing::copy_lint_libs(&root);
        let body = std::fs::read_to_string(repo_root().join(LINT))
            .unwrap_or_else(|e| panic!("read {LINT}: {e}"));
        scratch::write_exec(&root.join(LINT), &body);
        scratch::create_dir(&root.join("crates/x/src"));
        scratch::create_dir(&root.join("infra/x"));
        scratch::create_dir(&root.join("infra/oss-quickstart"));
        scratch::write_file(
            &root.join("crates/x/src/lib.rs"),
            "pub const Y: &str = include_str!(\"../../../infra/x/y.toml\");\n",
        );
        scratch::write_file(&root.join("infra/x/y.toml"), "tier = 1\n");
        scratch::write_file(&root.join("infra/oss-quickstart/Dockerfile"), dockerfile);
        Tree(root)
    }

    fn run(&self) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The shape of the real Dockerfile: a cargo stage that copies a
/// subset, then a runtime stage that copies what services read.
fn dockerfile(cargo_stage_copies: &str, runtime_copies: &str) -> String {
    format!(
        "FROM rust:slim AS rust-build\n\
         WORKDIR /build\n\
         COPY Cargo.toml Cargo.lock ./\n\
         COPY crates ./crates\n\
         {cargo_stage_copies}\
         RUN --mount=type=cache,target=/build/target \\\n\
         \x20   cargo build --release --workspace --bins\n\
         \n\
         FROM debian:slim AS runtime\n\
         COPY --from=rust-build /out/ /usr/local/bin/\n\
         {runtime_copies}"
    )
}

#[test]
fn the_file_copied_in_the_cargo_stage_is_carried() {
    let tree = Tree::new(
        "file-in-cargo-stage",
        &dockerfile("COPY infra/x/y.toml ./infra/x/y.toml\n", ""),
    );
    let out = tree.run();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("scanned 1 out-of-crate path"),
        "{}",
        text(&out)
    );
}

#[test]
fn an_ancestor_copied_in_the_cargo_stage_is_carried() {
    let tree = Tree::new(
        "ancestor-in-cargo-stage",
        &dockerfile("COPY infra ./infra\n", ""),
    );
    let out = tree.run();
    assert!(out.status.success(), "{}", text(&out));
}

/// The train #473 shape: the runtime stage copies the whole directory
/// for the services; rustc, in the earlier stage, never sees it.
#[test]
fn a_runtime_stage_copy_is_not_coverage() {
    let tree = Tree::new(
        "runtime-only",
        &dockerfile("", "COPY infra/x /opt/boss/infra/x\n"),
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let t = text(&out);
    assert!(
        t.contains("a crate reads infra/x/y.toml at build time"),
        "{t}"
    );
    assert!(t.contains("runtime stage does not count"), "{t}");
    assert!(t.contains("COPY infra/x/y.toml ./infra/x/y.toml"), "{t}");
}

#[test]
fn a_file_copied_nowhere_is_red() {
    let tree = Tree::new("nowhere", &dockerfile("", ""));
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("a crate reads infra/x/y.toml at build time"),
        "{}",
        text(&out)
    );
}

/// The collector's formatted includes put the literal on the next line.
/// The old line-oriented scanner silently omitted both real artifacts.
fn multiline_tree(tag: &str, cargo_copies: &str, runtime_copies: &str) -> Tree {
    let tree = Tree::new(tag, &dockerfile(cargo_copies, runtime_copies));
    scratch::write_file(
        &tree.0.join("crates/x/src/lib.rs"),
        "pub const Y: &[u8] = include_bytes!(\n    \"../../../infra/x/y.toml\"\n);\n\
         pub const Z: &str = include_str! (\n    \"../../../infra/x/z.json\",\n);\n",
    );
    scratch::write_file(&tree.0.join("infra/x/z.json"), "{}\n");
    tree
}

#[test]
fn multiline_literal_includes_missing_from_cargo_are_red() {
    let tree = multiline_tree("multiline-missing", "", "");
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    for path in ["infra/x/y.toml", "infra/x/z.json"] {
        assert!(
            text(&out).contains(&format!("a crate reads {path} at build time")),
            "{}",
            text(&out)
        );
    }
}

#[test]
fn multiline_literal_includes_in_runtime_only_are_red() {
    let tree = multiline_tree("multiline-runtime", "", "COPY infra ./infra\n");
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    for path in ["infra/x/y.toml", "infra/x/z.json"] {
        assert!(
            text(&out).contains(&format!("a crate reads {path} at build time")),
            "{}",
            text(&out)
        );
    }
}

#[test]
fn multiline_literal_includes_copied_into_cargo_are_carried() {
    for (tag, copies) in [
        (
            "multiline-files",
            "COPY infra/x/y.toml ./infra/x/y.toml\nCOPY infra/x/z.json ./infra/x/z.json\n",
        ),
        ("multiline-ancestor", "COPY infra ./infra\n"),
    ] {
        let tree = multiline_tree(tag, copies, "");
        let out = tree.run();
        assert!(out.status.success(), "{}", text(&out));
        assert!(
            text(&out).contains("scanned 2 out-of-crate path"),
            "{}",
            text(&out)
        );
    }
}

#[test]
fn an_unavailable_literal_matcher_refuses_instead_of_hiding_missing_copies() {
    let tree = multiline_tree(
        "matcher-refusal",
        "COPY infra/x/y.toml ./infra/x/y.toml\n",
        "",
    );
    scratch::write_file(
        &tree.0.join("crates/x/build.rs"),
        "const PATH: &str = \"../../infra/x/y.toml\";\n",
    );
    scratch::create_dir(&tree.0.join("bin"));
    scratch::write_exec(
        &tree.0.join("bin/grep"),
        "#!/bin/sh\ncase \"$1\" in *P*) echo 'literal matcher unavailable' >&2; exit 2;; esac\nexec /usr/bin/grep \"$@\"\n",
    );
    let out = Command::new("bash")
        .arg(tree.0.join(LINT))
        .current_dir(&tree.0)
        .env(
            "PATH",
            format!(
                "{}:{}",
                tree.0.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("literal matcher unavailable"),
        "{}",
        text(&out)
    );
    assert!(
        !text(&out).contains("every out-of-crate path"),
        "{}",
        text(&out)
    );
}

#[test]
fn a_valid_empty_literal_match_is_not_a_reader_failure() {
    let tree = Tree::new("build-only", &dockerfile("COPY infra ./infra\n", ""));
    scratch::write_file(
        &tree.0.join("crates/x/src/lib.rs"),
        "pub const Y: u8 = 1;\n",
    );
    scratch::write_file(
        &tree.0.join("crates/x/build.rs"),
        "const PATH: &str = \"../../infra/x/y.toml\";\n",
    );
    let out = tree.run();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("scanned 1 out-of-crate path"),
        "{}",
        text(&out)
    );
}

#[test]
fn unavailable_build_or_coverage_readers_cannot_issue_a_clean_verdict() {
    for reader in ["-ho", "-qE"] {
        let tree = Tree::new("reader-refusal", &dockerfile("COPY infra ./infra\n", ""));
        scratch::write_file(
            &tree.0.join("crates/x/build.rs"),
            "const PATH: &str = \"../../infra/x/y.toml\";\n",
        );
        scratch::create_dir(&tree.0.join("bin"));
        scratch::write_exec(
            &tree.0.join("bin/grep"),
            &format!(
                "#!/bin/sh\nif [ \"$1\" = '{reader}' ]; then echo 'reader unavailable' >&2; exit 2; fi\nexec /usr/bin/grep \"$@\"\n"
            ),
        );
        let out = Command::new("bash")
            .arg(tree.0.join(LINT))
            .current_dir(&tree.0)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    tree.0.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(!out.status.success(), "{reader}: {}", text(&out));
        assert!(text(&out).contains("reader unavailable"), "{}", text(&out));
        assert!(
            !text(&out).contains("every out-of-crate path"),
            "{}",
            text(&out)
        );
    }
}

/// A Dockerfile with no stage running cargo is one this lint cannot
/// read; it says so instead of finding every path uncovered — or, in
/// an earlier shape, covered by any COPY anywhere.
#[test]
fn a_dockerfile_without_a_cargo_stage_is_red_by_name() {
    let tree = Tree::new(
        "no-cargo-stage",
        "FROM debian:slim AS runtime\nCOPY infra ./infra\n",
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out)
            .contains("no stage of infra/oss-quickstart/Dockerfile both runs cargo and COPYs"),
        "{}",
        text(&out)
    );
}

/// The real Dockerfile passes the real lint — the pin that this car's
/// COPY line is where rustc reads from, not just somewhere in the file.
#[test]
fn the_repo_dockerfile_carries_every_reach_in_its_cargo_stage() {
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
        .current_dir(repo_root())
        .output()
        .expect("run the lint in the repo");
    assert!(out.status.success(), "{}", text(&out));
}
