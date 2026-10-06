//! `infra/lint/ci-images-are-pruned-by-age.sh` is RUN with the host's
//! machine token and an estate `BOSS_JOBS_URL` in its environment, and
//! must still pass: its fixtures are the lint's, never the host's.
//!
//! THE CLASS (backlog 920524dc, 2026-10-01). Every train was held from
//! 00:04Z: the conductor's consist check refused each board on this lint
//! with 3 FAILs. `infra/forge/landed-train-shas.lib.sh` makes its
//! machine-token header WHEN SOURCED, for the `BOSS_JOBS_URL` it finds,
//! and the lint's `resolve()` sources it inside `$( ( … ) )` — a subshell,
//! where `infra/lib/secret-header.sh` refuses a first call (ORDER). That
//! only bites where a token is mounted AND the URL names an estate host,
//! which became true in the conductor pod at the first `boss-machine-token`
//! mint (rotation ace0521e, 2026-09-30 22:59Z) — so the lint was green in
//! every gate and red in the one place that judges the assembled tree.
//! The production library is right (the sweep sources it from its own
//! shell); the lint was reading the host. These legs plant both shapes the
//! host can hand it: the hosts list in the environment, and the hosts list
//! only in the rendered sor.env.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::Path;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/ci-images-are-pruned-by-age.sh";

/// A token-shaped stand-in: 43 characters, the length the broker mints.
fn token_dir(tag: &str) -> std::path::PathBuf {
    let dir = scratch::scratch_dir(&format!("image-prune-hermetic-{tag}"));
    scratch::write_file(&dir.join("current"), &format!("{}\n", "t".repeat(43)));
    dir
}

fn run(envs: &[(&str, &Path)], strs: &[(&str, &str)], unset: &[&str]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(LINT)).current_dir(repo_root());
    for k in unset {
        cmd.env_remove(k);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    for (k, v) in strs {
        cmd.env(k, v);
    }
    cmd.output()
        .unwrap_or_else(|e| panic!("could not run {LINT}: {e}"))
}

fn assert_green(out: &Output, leg: &str) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{LINT} is not hermetic ({leg}): the host's token and system of record reached its \
         fixtures, the shape that held every train on 2026-10-01 (backlog 920524dc)\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn the_lint_passes_with_a_mounted_token_and_an_estate_url_in_the_environment() {
    let dir = token_dir("env-hosts");
    let out = run(
        &[("BOSS_MACHINE_TOKEN_DIR", &dir)],
        &[
            ("BOSS_JOBS_URL", "http://10.20.0.34:7900"),
            ("BOSS_MACHINE_TOKEN_HOSTS", "10.20.0.34"),
        ],
        &[],
    );
    assert_green(&out, "hosts list in the environment");
}

#[test]
fn the_lint_passes_when_the_hosts_list_comes_only_from_the_rendered_sor_env() {
    let dir = token_dir("sor-env");
    let sor_env = dir.join("sor.env");
    scratch::write_file(
        &sor_env,
        "BOSS_JOBS_URL=http://10.20.0.34:7900\nBOSS_MACHINE_TOKEN_HOSTS=10.20.0.34\n",
    );
    let out = run(
        &[("BOSS_MACHINE_TOKEN_DIR", &dir), ("BOSS_SOR_ENV", &sor_env)],
        &[("BOSS_JOBS_URL", "http://10.20.0.34:7900")],
        &["BOSS_MACHINE_TOKEN_HOSTS"],
    );
    assert_green(&out, "hosts list only in sor.env");
}
