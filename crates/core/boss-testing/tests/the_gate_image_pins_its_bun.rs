//! The gate image's bun is ONE bun, named, and the build refuses another.
//!
//! THE DEFECT (backlog e30098d3, review 2f67531c of car a4d3f84e). The
//! gate's bun install cache lives on the shared seed volume since
//! 4bbf875d, and its safety argument — a package directory appears whole
//! or not at all — is a property of bun's own source (bun-v1.3.14
//! extract_tarball.zig, sys.zig), read at one version. The bun that
//! implements it came from `oven/bun:1.3-slim`, a FLOATING tag: the
//! mirror (infra/forge/mirror-base-images.sh) copies whatever that tag
//! names on the day it runs, and the Dockerfile took whatever the forge
//! tag then served, so the next full mirror run could change the cache's
//! implementation under every gate with no line of this tree changing.
//!
//! THE FIX, in two halves. The mirror copies bun from ONE manifest
//! digest, so its run cannot move the forge tag to another bun; and the
//! image build asks the bun it copied for its revision and refuses any
//! other, which is the check at the consuming layer — it also catches a
//! forge tag moved by anything that is not the mirror. The forge tag
//! itself keeps its name: a new tag would have to be mirrored BEFORE the
//! train that references it, and the train's build-image job pulls it
//! from the forge before any converge has run the mirror.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::fs;
use std::process::Command;

const DOCKERFILE: &str = "infra/forge/boss-ci/Dockerfile";
const MIRROR: &str = "infra/forge/mirror-base-images.sh";
const REVISION_CHECK: &str = "RUN got=\"$(bun --revision)\" && [ \"$got\" = \"";

fn read(path: &str) -> String {
    fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The bun stage's forge tag, e.g. `1.3-slim`, off the Dockerfile's FROM.
fn bun_stage_tag(dockerfile: &str) -> String {
    let from = dockerfile
        .lines()
        .find(|l| l.starts_with("FROM ") && l.ends_with(" AS bun"))
        .unwrap_or_else(|| panic!("{DOCKERFILE} has no `FROM … AS bun` stage"));
    let image = from.trim_start_matches("FROM ").trim_end_matches(" AS bun");
    let (_, tag) = image
        .rsplit_once("/bun:")
        .unwrap_or_else(|| panic!("the bun stage is not a forge `bun:` tag: {from}"));
    tag.to_string()
}

/// The ONE revision the image build accepts, off its check line.
fn pinned_revision(dockerfile: &str) -> String {
    let line = dockerfile
        .lines()
        .find(|l| l.starts_with(REVISION_CHECK))
        .unwrap_or_else(|| {
            panic!("{DOCKERFILE} must check the copied bun's revision with a line starting {REVISION_CHECK}")
        });
    let rest = &line[REVISION_CHECK.len()..];
    let (revision, _) = rest
        .split_once("\" ]")
        .unwrap_or_else(|| panic!("the revision check names no revision: {line}"));
    revision.to_string()
}

/// The mirror copies bun from one manifest digest, never a tag, and the
/// forge tag it writes is the one the Dockerfile's bun stage pulls.
#[test]
fn the_mirror_copies_bun_from_one_digest() {
    let mirror = read(MIRROR);
    let tag = bun_stage_tag(&read(DOCKERFILE));
    let dest = format!("|bun:{tag}");
    let lines: Vec<&str> = mirror.lines().filter(|l| l.ends_with(&dest)).collect();
    assert_eq!(
        lines.len(),
        1,
        "{MIRROR} must map exactly one source to the forge tag bun:{tag}: {lines:?}"
    );
    let source = lines[0].trim_end_matches(&dest);
    let digest = source
        .strip_prefix("docker.io/oven/bun@sha256:")
        .unwrap_or_else(|| {
            panic!(
                "{MIRROR} copies bun from `{source}`, a name that can move; copy it from \
                 docker.io/oven/bun@sha256:<the manifest digest> so a mirror run cannot change \
                 the gate's bun"
            )
        });
    assert!(
        digest.len() == 64
            && digest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "not a sha256 digest: {source}"
    );
}

/// The pinned revision is a bun the stage's tag can serve: `1.3-slim`
/// serves 1.3.x, so a pin of another minor is a typo that would red
/// every train's build-image.
#[test]
fn the_pinned_revision_is_one_the_stage_tag_can_serve() {
    let dockerfile = read(DOCKERFILE);
    let tag = bun_stage_tag(&dockerfile);
    let revision = pinned_revision(&dockerfile);
    let series = tag.split('-').next().unwrap_or_default();
    assert!(
        revision.starts_with(&format!("{series}.")) && revision.contains('+'),
        "the bun stage pulls bun:{tag}, so the pinned revision must be {series}.<patch>+<commit>, \
         as `bun --revision` prints it; got {revision}"
    );
    let copy = dockerfile
        .find("COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun")
        .expect("the image copies bun out of the bun stage");
    let check = dockerfile
        .find(REVISION_CHECK)
        .expect("the image checks bun's revision");
    assert!(
        copy < check,
        "the revision check must run after bun is copied in"
    );
}

/// The check as it ships, run against a stand-in bun: the pinned
/// revision builds, any other refuses and says what it found.
#[test]
fn the_image_build_refuses_any_other_bun() {
    let dockerfile = read(DOCKERFILE);
    let revision = pinned_revision(&dockerfile);
    let command = dockerfile
        .lines()
        .find(|l| l.starts_with(REVISION_CHECK))
        .map(|l| l.trim_start_matches("RUN ").to_string())
        .expect("the image checks bun's revision");
    let dir = scratch_dir("the-gate-image-pins-its-bun");
    let run = |prints: &str| {
        write_exec(
            &dir.join("bun"),
            &format!("#!/bin/sh\n[ \"$1\" = --revision ] || exit 64\necho '{prints}'\n"),
        );
        Command::new("sh")
            .args(["-c", &command])
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .output()
            .expect("sh")
    };
    let pinned = run(&revision);
    let other = run("1.3.15+feedfaced");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        pinned.status.success(),
        "the pinned bun must build: {}",
        String::from_utf8_lossy(&pinned.stderr)
    );
    assert!(!other.status.success(), "another bun must refuse the build");
    let said = String::from_utf8_lossy(&other.stderr);
    assert!(
        said.contains("1.3.15+feedfaced"),
        "the refusal names the bun it found: {said}"
    );
}
