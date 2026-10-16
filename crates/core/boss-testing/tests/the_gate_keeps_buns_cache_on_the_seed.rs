//! Bun's download cache lives on the seed volume beside cargo's
//! (backlog 4bbf875d).
//!
//! MEASURED on origin/main@5cfe679a, 2026-10-16. The gate container's
//! HOME is /gate-target (gate-runner.yaml), this pod's own workspace,
//! emptied at exit — so bun's install cache ($HOME/.bun/install/cache)
//! started empty on every gate. Until car ade746a1 the silenced warm-up
//! in run.sh filled it at minute 0; with that warm-up removed, the first
//! package download in a non-web `--auto` gate is svelte-check's
//! install, ~40 minutes in, and an npm registry blip there costs the
//! whole gate. Cargo had the same exposure and was fixed by putting
//! CARGO_HOME on /gate-seed (2026-08-27, a green branch recorded red by
//! one dropped crates.io fetch).
//!
//! So run.sh exports BUN_INSTALL_CACHE_DIR on the seed in the same
//! block that exports CARGO_HOME, falling back to the workspace when the
//! seed is not mounted. The block is lifted out of run.sh and RUN, and
//! bun itself is asked which cache it would use under the environment
//! the block leaves — `bun pm cache` — because a misspelt variable is
//! silence: bun ignores a name it does not read and every gate would go
//! on downloading cold, green.
//!
//! THE BLOCK ALSO SWEEPS bun's `<cache>/.tmp` (review 2f67531c, B1).
//! With the cache on another device from /tmp, bun unpacks there, and
//! every install leaves three entries behind (~2.4 MB, ~617 inodes,
//! measured by run 797f3b9b) that nothing reclaims — harmless in a
//! per-run HOME, unbounded on the seed. The sweep takes entries whose
//! CTIME is older than the gate Job can live, and the tests below hold
//! that age to the Job's deadline, show it removes only stale entries,
//! and show a directory with an old mtime and a fresh ctime — what
//! RENAME_EXCHANGE leaves while another install may read it — is kept.

use boss_testing::repo_root;
use std::path::Path;
use std::process::Command;

const OPEN: &str = "# --- download caches (begin) ---";
const CLOSE: &str = "# --- download caches (end) ---";

fn run_sh() -> String {
    let path = repo_root().join("infra/gate-runner/run.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The marked block, comments and all (bash ignores them).
fn block() -> String {
    let sh = run_sh();
    let start = sh.find(OPEN).unwrap_or_else(|| {
        panic!(
            "infra/gate-runner/run.sh has no `{OPEN}` block — nothing puts bun's \
             download cache on the seed volume, so every gate starts it empty"
        )
    });
    let end = sh[start..]
        .find(CLOSE)
        .unwrap_or_else(|| panic!("run.sh's download caches block is not closed by `{CLOSE}`"));
    sh[start..start + end].to_string()
}

/// Run the block with `$SEED` at `seed`, then `extra`; (status, stdout, stderr).
fn run_block(seed: &Path, prelude: &str, extra: &str, cwd: &Path) -> (i32, String, String) {
    run_text(&block(), seed, prelude, extra, cwd)
}

/// Run `text` (the block, or the block with its age rewritten) as
/// run.sh would: under `set -euo pipefail`, `$SEED` at `seed`.
fn run_text(
    text: &str,
    seed: &Path,
    prelude: &str,
    extra: &str,
    cwd: &Path,
) -> (i32, String, String) {
    let script = format!(
        "set -euo pipefail\nSEED=\"$1\"\n{prelude}\n{text}\n\
         printf 'CARGO_HOME=%s\\n' \"$(printenv CARGO_HOME)\"\n\
         printf 'BUN_INSTALL_CACHE_DIR=%s\\n' \"$(printenv BUN_INSTALL_CACHE_DIR)\"\n{extra}\n"
    );
    let out = Command::new("bash")
        .args(["-c", &script, "download-caches"])
        .arg(seed)
        .current_dir(cwd)
        .env_remove("BUN_INSTALL_CACHE_DIR")
        .env_remove("CARGO_HOME")
        .output()
        .expect("run bash");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn value<'a>(stdout: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key}=");
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("the block left no {key} line; stdout:\n{stdout}"))
}

/// WITH THE SEED MOUNTED, BUN'S CACHE IS ON IT — and bun agrees. The
/// last line of the script is bun's own answer to "which cache would
/// you install from", asked in a directory holding a package.json (the
/// command refuses without one) under exactly the environment the block
/// exported. It names the seed's directory, so the gate's `bun install`
/// reuses what every earlier gate downloaded.
#[test]
fn with_the_seed_mounted_bun_installs_from_a_cache_on_it() {
    let dir = boss_testing::scratch_dir("gate-bun-cache-seeded");
    let seed = dir.join("seed");
    boss_testing::create_dir(&seed);
    let project = dir.join("project");
    boss_testing::create_dir(&project);
    boss_testing::write_file(&project.join("package.json"), "{\"name\":\"probe\"}\n");

    let (code, stdout, stderr) =
        run_block(&seed, "", "echo \"BUN_SAYS=$(bun pm cache)\"", &project);
    assert_eq!(
        code, 0,
        "the block (then `bun pm cache`) failed:\n{stdout}\n{stderr}"
    );

    let cache = value(&stdout, "BUN_INSTALL_CACHE_DIR");
    let seed_str = seed.to_string_lossy();
    assert!(
        cache.starts_with(&format!("{seed_str}/")),
        "BUN_INSTALL_CACHE_DIR is `{cache}`, not under the seed {seed_str} — \
         bun's cache would die with the pod"
    );
    assert!(
        Path::new(cache).is_dir(),
        "the block must create bun's cache directory ({cache}) as it creates CARGO_HOME"
    );
    assert_eq!(
        value(&stdout, "BUN_SAYS"),
        cache,
        "bun does not read the cache the block exported — the variable is \
         misspelt or overridden, and every gate would still download cold"
    );
    assert_eq!(
        value(&stdout, "CARGO_HOME"),
        format!("{seed_str}/cargo"),
        "CARGO_HOME moved off the seed volume in the same block"
    );
    assert!(
        stdout.contains(&format!("BUN_INSTALL_CACHE_DIR={cache}"))
            && stdout
                .lines()
                .any(|l| l.starts_with("gate-runner: ") && l.contains(cache)),
        "the runner's log must say where bun's cache is, so a cold gate is \
         readable as one; stdout:\n{stdout}"
    );
    // A first gate on a new seed has no .tmp yet: the sweep's `find`
    // fails on it, and under `set -euo pipefail` that must not be fatal.
    assert!(
        stdout.contains("(swept 0 stale .tmp entries)"),
        "with no .tmp to sweep the block must say it swept nothing; stdout:\n{stdout}"
    );
}

/// The block's sweep age, in minutes — the `N` of its `-cmin +N`.
fn sweep_minutes(block: &str) -> u64 {
    let after = block.split("-cmin +").nth(1).unwrap_or_else(|| {
        panic!(
            "the download caches block has no `-cmin +N` sweep of bun's .tmp — \
             every install leaks ~2.4 MB and ~620 inodes onto the seed for good \
             (review 2f67531c, B1)"
        )
    });
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("`-cmin +{digits}` is not a whole number of minutes: {e}"))
}

/// The gate Job's activeDeadlineSeconds, read from its manifest.
fn gate_deadline_seconds() -> u64 {
    let path = repo_root().join("infra/gate-runner/gate-runner.yaml");
    let yaml =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let line = yaml
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("activeDeadlineSeconds:"))
        .unwrap_or_else(|| panic!("{} declares no activeDeadlineSeconds", path.display()));
    let value = line.trim_start_matches("activeDeadlineSeconds:").trim();
    value
        .parse()
        .unwrap_or_else(|e| panic!("activeDeadlineSeconds `{value}` is not a number: {e}"))
}

/// NO LIVE INSTALL CAN OWN WHAT THE SWEEP REMOVES. A .tmp entry may be
/// an extract in progress, or a directory RENAME_EXCHANGE swapped out
/// while another install still copies from it — and a gate's install
/// lives no longer than its Job. So the sweep's age must exceed the
/// Job's deadline: two facts in two files, held apart here so neither
/// moves alone (a deadline raised past four hours would let a sweep
/// delete under a running gate).
#[test]
fn the_sweep_waits_longer_than_any_gate_can_run() {
    let minutes = sweep_minutes(&block());
    let deadline = gate_deadline_seconds();
    assert!(
        minutes * 60 > deadline,
        "the bun .tmp sweep removes entries older than {minutes} min, but a gate \
         may run {deadline} s (gate-runner.yaml activeDeadlineSeconds) — a sweep \
         could delete a directory a live install is still copying out of"
    );
}

/// THE SWEEP REMOVES ONLY STALE .tmp ENTRIES. bun's .tmp under the
/// seed's cache collects a patched copy, an unpatched copy and a
/// .node-gyp directory per install, and nothing else reclaims them.
///
/// ctime cannot be set from a test, so the old entries are made OLD by
/// waiting: they are created, the test sleeps past a threshold of six
/// seconds, and the block runs with its `-cmin +N` rewritten to that
/// threshold (`+0.1` minutes; GNU find takes a fraction). The fresh
/// entries are made by the script's own prelude, milliseconds before the
/// sweep, and one of them has a two-day-old MTIME, as a directory swapped
/// out by RENAME_EXCHANGE does. The block runs once AS WRITTEN first: at
/// its real age nothing that young is touched.
#[test]
fn the_sweep_removes_only_stale_tmp_entries() {
    let dir = boss_testing::scratch_dir("gate-bun-cache-sweep");
    let seed = dir.join("seed");
    let cache = seed.join("bun-install-cache");
    let tmp = cache.join(".tmp");
    // A cached package (outside .tmp) and two leaked entries, all OLD.
    let package = cache.join("react@18.3.1@@@1");
    boss_testing::create_dir(&package);
    boss_testing::write_file(&package.join("package.json"), "{\"name\":\"react\"}\n");
    let old_dir = tmp.join("swapped-out-xyflow");
    boss_testing::create_dir(&old_dir.join("dist"));
    boss_testing::write_file(&old_dir.join("dist/index.js"), "old\n");
    let old_gyp = tmp.join(".node-gyp-old");
    boss_testing::create_dir(&old_gyp);

    // AS WRITTEN (240 minutes): seconds-old entries all survive.
    let (code, stdout, stderr) = run_block(&seed, "", "", &dir);
    assert_eq!(code, 0, "the block failed:\n{stdout}\n{stderr}");
    assert!(
        old_dir.is_dir() && old_gyp.is_dir() && package.is_dir(),
        "the sweep as written removed entries seconds old — a live install could \
         own them; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("(swept 0 stale .tmp entries)"),
        "nothing was stale, so the log must say 0; stdout:\n{stdout}"
    );

    std::thread::sleep(std::time::Duration::from_secs(9));

    let as_written = block();
    let minutes = sweep_minutes(&as_written);
    let aged = as_written.replacen(&format!("-cmin +{minutes}"), "-cmin +0.1", 1);
    assert_ne!(aged, as_written, "could not rewrite the sweep's age");
    let fresh_dir = tmp.join("extract-in-progress");
    let fresh_file = tmp.join("fresh-manifest.tmp");
    // Just swapped out by RENAME_EXCHANGE: its mtime is the old copy's
    // (two days here), its ctime is now. An -mmin sweep would take it
    // while another install may still be copying out of it.
    let swapped = tmp.join("just-swapped-out");
    let prelude = format!(
        "mkdir -p '{}' '{}' && : > '{}' && touch -d '2 days ago' '{}'",
        fresh_dir.display(),
        swapped.display(),
        fresh_file.display(),
        swapped.display()
    );
    let (code, stdout, stderr) = run_text(&aged, &seed, &prelude, "", &dir);
    assert_eq!(code, 0, "the aged block failed:\n{stdout}\n{stderr}");

    assert!(
        !old_dir.exists() && !old_gyp.exists(),
        "the stale .tmp entries survived the sweep; stdout:\n{stdout}"
    );
    assert!(
        fresh_dir.is_dir() && fresh_file.is_file(),
        "the sweep removed a FRESH .tmp entry — one a live install may own; \
         stdout:\n{stdout}"
    );
    assert!(
        swapped.is_dir(),
        "the sweep removed a directory whose MTIME is old but whose CTIME is \
         fresh — exactly what RENAME_EXCHANGE leaves while another install may \
         still read it. Age by -cmin, not -mmin; stdout:\n{stdout}"
    );
    assert!(
        package.join("package.json").is_file() && tmp.is_dir(),
        "the sweep reached outside .tmp's entries (a cached package, or .tmp \
         itself); stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("(swept 2 stale .tmp entries)"),
        "the log must count what the sweep removed; stdout:\n{stdout}"
    );
}

/// WITHOUT THE SEED (an old manifest rendered against this script),
/// bun's cache falls back to the workspace beside CARGO_HOME — cold and
/// loud, never dead. `mkdir` is shadowed so the test does not write
/// /gate-target on the machine running it; the paths it would make are
/// read back instead.
#[test]
fn without_the_seed_both_caches_fall_back_to_the_workspace() {
    let dir = boss_testing::scratch_dir("gate-bun-cache-unseeded");
    let seed = dir.join("absent");
    let (code, stdout, stderr) =
        run_block(&seed, "mkdir() { printf 'MKDIR=%s\\n' \"$*\"; }", "", &dir);
    assert_eq!(
        code, 0,
        "the block failed without a seed:\n{stdout}\n{stderr}"
    );
    let cache = value(&stdout, "BUN_INSTALL_CACHE_DIR");
    assert!(
        cache.starts_with("/gate-target/"),
        "without the seed bun's cache must land in the per-run workspace, got `{cache}`"
    );
    assert_eq!(value(&stdout, "CARGO_HOME"), "/gate-target/cargo");
    assert!(
        value(&stdout, "MKDIR").contains(cache),
        "the fallback must still create bun's cache directory; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("running cold"),
        "the fallback must say it is running cold; stdout:\n{stdout}"
    );
}

/// THE CACHE IS CHOSEN BEFORE ANYTHING INSTALLS. An export after the
/// first `bun install` (the warm-up while it lasts, gate.sh's `web
/// install` always) would leave that install on the empty per-run cache.
#[test]
fn the_cache_is_chosen_before_the_runner_installs_or_gates() {
    let sh = run_sh();
    let at = sh
        .find(OPEN)
        .unwrap_or_else(|| panic!("run.sh has no `{OPEN}` block"));
    let gate = sh
        .find("./infra/gate.sh ${GATE_MODE")
        .expect("run.sh runs ./infra/gate.sh ${GATE_MODE:-}");
    assert!(
        at < gate,
        "the download caches block must come before the gate runs"
    );
    let first_install = sh
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .find(|l| l.contains("bun install"))
        .and_then(|l| sh.find(l));
    if let Some(install) = first_install {
        assert!(
            at < install,
            "run.sh runs `bun install` before it chooses bun's cache"
        );
    }
}
