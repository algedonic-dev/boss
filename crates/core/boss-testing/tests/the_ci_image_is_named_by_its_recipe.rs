//! The CI runner image is named by its RECIPE, not by the commit that
//! asked for it, and a recipe already in the registry is not built again.
//!
//! WHY (backlog 399ee6c3, found by the factory-speed research session of
//! 2026-10-15). `build-image` tagged `boss-ci:${{ github.sha }}` on every
//! train and ran kaniko every time, so 780 trains in 09-17..10-15 each
//! pushed a 3.48 GB image to the forge registry and pulled it into the
//! system docker daemon, whose disk floor is a named reason trains are
//! rate-limited. The recipe changed seven times in those two months.
//! Meanwhile the stamp — sha256 of `infra/forge/boss-ci/Dockerfile`
//! followed by `required-tools.txt` — already identified the image's
//! content: the Dockerfile bakes it into `/etc/boss-ci-stamp` and
//! `locomotive.sh` reds when it disagrees with the tree.
//!
//! A stamp tag keeps what the per-commit tag was added for (6aa603ef: a
//! floating `:rust1.96` was served stale from the runner's cache and
//! failed the stamp check): a runner's local `boss-ci:<stamp>` can only
//! hold an image built from that stamp's recipe, so it cannot be stale
//! in the one way the locomotive checks.
//!
//! These tests EXECUTE the workflow's own `run:` blocks — read out of
//! `.forgejo/workflows/ci.yml`, never a copy — against a stubbed registry
//! (`wget`) and a stubbed `/kaniko/executor`, because nothing else runs
//! `build-image` before a train does (the gate never has).

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::{Command, Output};

const STAMP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";
const IMAGE: &str = "10.20.0.15:3000/david/boss-ci";

fn workflow() -> String {
    let path = repo_root().join(".forgejo/workflows/ci.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// A job's lines: from `  <name>:` to the next line at two-space indent
/// (the next job, or the comment block that introduces it).
fn job(workflow: &str, name: &str) -> String {
    let header = format!("  {name}:");
    let mut lines = workflow.lines().skip_while(|l| l.trim_end() != header);
    assert!(
        lines.next().is_some(),
        "no `{name}` job in .forgejo/workflows/ci.yml — if it was renamed, rename it here too"
    );
    lines
        .take_while(|l| l.trim().is_empty() || l.starts_with("   "))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// The `run: |` block of the step named `step` inside `job`, dedented.
fn run_block(job: &str, step: &str) -> String {
    let mut lines = job
        .lines()
        .skip_while(|l| l.trim() != format!("- name: {step}"));
    assert!(lines.next().is_some(), "no step named `{step}`");
    let run = lines
        .find(|l| l.trim_start().starts_with("run: |"))
        .unwrap_or_else(|| panic!("step `{step}` has no `run: |` block"));
    let depth = run.len() - run.trim_start().len();
    let body: Vec<&str> = lines
        .take_while(|l| l.trim().is_empty() || l.len() - l.trim_start().len() > depth)
        .collect();
    let indent = body
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    body.iter()
        .map(|l| format!("{}\n", l.get(indent..).unwrap_or("")))
        .collect()
}

/// A stubbed registry and executor, and the build-image script pointed at
/// them. `manifests` maps a tag to the manifest body the registry serves.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(token: bool, manifests: &[(&str, &str)]) -> Fixture {
        let root = scratch_dir("ci-image-recipe");
        let fix = root.join("fix");
        for d in ["bin", "fix", "kaniko/.docker"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        if token {
            write_file(&fix.join("token"), "{\"token\":\"pull-token\"}");
        }
        for (tag, body) in manifests {
            write_file(&fix.join(format!("manifest-{tag}")), body);
        }
        // The registry. Every call is logged; a manifest read that does not
        // carry the bearer the token endpoint handed out is refused, as the
        // forge refuses it.
        write_exec(
            &root.join("bin/wget"),
            &format!(
                "#!/bin/sh\nfix='{}'\nfor a; do url=\"$a\"; done\nprintf '%s\\n' \"$*\" >> \"$fix/wget.log\"\n\
                 case \"$url\" in\n\
                 */v2/token*) [ -f \"$fix/token\" ] || exit 1; cat \"$fix/token\" ;;\n\
                 */manifests/*) case \"$*\" in *'Authorization: Bearer pull-token'*) ;; *) exit 1 ;; esac\n\
                 tag=\"${{url##*/}}\"; [ -f \"$fix/manifest-$tag\" ] || exit 1; cat \"$fix/manifest-$tag\" ;;\n\
                 *) exit 1 ;;\n\
                 esac\n",
                fix.display()
            ),
        );
        write_exec(
            &root.join("kaniko/executor"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}/executor.args'\n",
                fix.display()
            ),
        );
        let script = run_block(
            &job(&workflow(), "build-image"),
            "Build and push the runner image",
        )
        .replace("/kaniko/", &format!("{}/kaniko/", root.display()));
        write_file(&root.join("build-image.sh"), &script);
        Fixture { root }
    }

    fn run(&self, stamp: &str) -> Output {
        Command::new("sh")
            .arg("-e")
            .arg(self.root.join("build-image.sh"))
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("TOKEN", "package-token")
            .env("GITHUB_SHA", SHA)
            .env("CI_STAMP", stamp)
            .output()
            .expect("run the build-image script")
    }

    fn executor_args(&self) -> Option<String> {
        std::fs::read_to_string(self.fix().join("executor.args")).ok()
    }

    fn fix(&self) -> PathBuf {
        self.root.join("fix")
    }
}

fn said(out: &Output) -> String {
    format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_recipe_the_registry_already_holds_is_not_built_again() {
    let f = Fixture::new(true, &[(STAMP, "{\"m\":1}"), ("rust1.96", "{\"m\":1}")]);
    let out = f.run(STAMP);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(
        f.executor_args(),
        None,
        "kaniko ran for a recipe whose image the registry already holds:\n{}",
        said(&out)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("boss-ci:{STAMP}")),
        "the skip must name the image the next jobs run in:\n{}",
        said(&out)
    );
}

#[test]
fn a_new_recipe_is_built_and_tagged_by_its_stamp_not_its_commit() {
    let f = Fixture::new(true, &[("rust1.96", "{\"m\":0}")]);
    let out = f.run(STAMP);
    assert!(out.status.success(), "{}", said(&out));
    let args = f
        .executor_args()
        .unwrap_or_else(|| panic!("kaniko did not run for a new recipe:\n{}", said(&out)));
    assert!(
        args.contains(&format!("{IMAGE}:{STAMP}")),
        "not tagged by its stamp:\n{args}"
    );
    assert!(
        args.contains(&format!("{IMAGE}:rust1.96")),
        "the floating tag the gate runner, the dev pod and the lighter route pull must still move:\n{args}"
    );
    // The commit still names the build CONTEXT (`--context …#<sha>`); it
    // must name no destination.
    assert!(
        !args.contains(&format!("{IMAGE}:{SHA}")),
        "a per-commit tag is the 3.48 GB-per-train image this removed:\n{args}"
    );
}

/// The floating tag moves on every build, so a train whose recipe was
/// built and then went red leaves `:rust1.96` on a recipe main does not
/// carry. Skipping whenever the stamp exists would leave it there until
/// the next recipe change; until 399ee6c3 the next train's rebuild healed
/// it, and it still does.
#[test]
fn a_floating_tag_on_another_recipe_is_rebuilt_back() {
    let f = Fixture::new(true, &[(STAMP, "{\"m\":1}"), ("rust1.96", "{\"m\":2}")]);
    let out = f.run(STAMP);
    assert!(out.status.success(), "{}", said(&out));
    let args = f.executor_args().unwrap_or_else(|| {
        panic!(
            "kaniko did not run though :rust1.96 serves another recipe:\n{}",
            said(&out)
        )
    });
    assert!(args.contains(&format!("{IMAGE}:rust1.96")), "{args}");
}

/// Every failure to READ the registry builds — today's behaviour, the
/// direction that costs a minute and never a red.
#[test]
fn a_registry_that_cannot_be_read_builds() {
    for (token, manifests) in [
        (false, vec![(STAMP, "{\"m\":1}"), ("rust1.96", "{\"m\":1}")]),
        (true, vec![]),
    ] {
        let f = Fixture::new(token, &manifests);
        let out = f.run(STAMP);
        assert!(out.status.success(), "{}", said(&out));
        assert!(
            f.executor_args().is_some(),
            "an unreadable registry skipped the build:\n{}",
            said(&out)
        );
    }
}

/// The stamp arrives from another job's output. An empty one (that job's
/// step did not run) or one of the wrong shape names no image the next
/// jobs could pull, so it fails HERE, naming why, rather than as a pull
/// error two jobs later.
#[test]
fn a_missing_or_malformed_stamp_fails_before_anything_is_built() {
    for bad in ["", SHA, &STAMP.to_uppercase(), &format!("{STAMP} ")] {
        let f = Fixture::new(true, &[]);
        let out = f.run(bad);
        assert!(
            !out.status.success(),
            "stamp {bad:?} was accepted:\n{}",
            said(&out)
        );
        assert_eq!(f.executor_args(), None, "stamp {bad:?}: kaniko ran");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("CI_STAMP"),
            "stamp {bad:?}: the failure must name the input:\n{}",
            said(&out)
        );
    }
}

/// THE STAMP IS ONE FACT COMPUTED FOUR TIMES — by the Dockerfile into
/// the image, by locomotive.sh from the checkout, by delivery-ci.py for
/// the lighter route, and by delivery-plan to name the image. The first
/// three are compared at run time; this pins the fourth to locomotive's
/// own line, on the real tree.
#[test]
fn delivery_plan_names_the_image_by_the_stamp_the_locomotive_checks() {
    let root = scratch_dir("ci-image-recipe-stamp");
    let output = root.join("github-output");
    write_file(&output, "");
    let block = run_block(
        &job(&workflow(), "delivery-plan"),
        "Name the runner image by its recipe",
    );
    let script = root.join("stamp.sh");
    write_file(&script, &block);
    let out = Command::new("sh")
        .arg("-e")
        .arg(&script)
        .current_dir(repo_root())
        .env("GITHUB_OUTPUT", &output)
        .output()
        .expect("run the stamp step");
    assert!(out.status.success(), "{}", said(&out));
    let named = std::fs::read_to_string(&output).unwrap();

    let locomotive =
        std::fs::read_to_string(repo_root().join("infra/forge/locomotive.sh")).unwrap();
    let want_line = locomotive
        .lines()
        .find(|l| l.starts_with("want=\"$("))
        .expect("locomotive.sh's `want=` stamp line");
    let check = Command::new("sh")
        .arg("-c")
        .arg(format!("{want_line}\nprintf 'ci_stamp=%s\\n' \"$want\""))
        .current_dir(repo_root())
        .output()
        .expect("run locomotive's stamp line");
    assert!(check.status.success(), "{}", said(&check));
    assert_eq!(
        named,
        String::from_utf8_lossy(&check.stdout),
        "delivery-plan names the image by a different stamp than the locomotive checks"
    );
}

/// The three jobs read the one name: build-image builds it, locomotive
/// and web run in it.
#[test]
fn the_image_the_jobs_run_in_is_the_one_build_image_names() {
    let wf = workflow();
    let plan = job(&wf, "delivery-plan");
    assert!(
        plan.contains("ci_stamp: ${{ steps.stamp.outputs.ci_stamp }}"),
        "delivery-plan does not export the stamp:\n{plan}"
    );
    assert!(plan.contains("id: stamp"), "{plan}");
    let build = job(&wf, "build-image");
    assert!(
        build.contains("CI_STAMP: ${{ needs.delivery-plan.outputs.ci_stamp }}"),
        "build-image does not read the stamp delivery-plan named:\n{build}"
    );
    for name in ["locomotive", "web"] {
        let body = job(&wf, name);
        assert!(
            body.contains(&format!(
                "image: {IMAGE}:${{{{ needs.delivery-plan.outputs.ci_stamp }}}}"
            )),
            "{name} does not run in the image named by its recipe:\n{body}"
        );
        assert!(
            body.contains("needs: [delivery-plan"),
            "{name} must need delivery-plan to read its output"
        );
    }
    assert!(
        !wf.contains("boss-ci:${{ github.sha }}") && !wf.contains("boss-ci:${GITHUB_SHA}"),
        "a per-commit boss-ci tag is back in ci.yml"
    );
}
