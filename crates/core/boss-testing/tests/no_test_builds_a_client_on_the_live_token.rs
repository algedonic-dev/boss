//! No test, in any crate, builds a client that stamps the process's
//! live machine token, and no test spawns the `boss` binary with the
//! token directory it would read in production (backlog 2ee29275, F2 —
//! car A's widening of the name scan car B added for boss-cli).
//!
//! WHAT WAS MEASURED (car B's run 6cb04ff7, 2026-09-29). The boss-cli
//! half of F2 sat inside `gate::machine_client`, but the same read was
//! everywhere else: the gateway's tests call
//! `MachineClient::build(reqwest::Client::builder())` dozens of times
//! (local_auth.rs, role_headers.rs, break_glass.rs, tests/*), and
//! `machine_token::Client::build` is called from test code in several
//! crates. Each one reads `machine_token::shared()` — the watched source
//! over the mounted Secret — and a failing test prints the request head
//! it captured, so the day design 6805c764 car 4 mounts the Secret where
//! a test runs, the estate token would be in that run's log. A
//! `#[cfg(test)]` switch inside boss-core cannot reach them: another
//! crate's tests compile boss-core WITHOUT its `cfg(test)`. So the
//! constructor is theirs to choose, and this pin holds the choice: a
//! test builds with `Client::unstamped` / `BlockingClient::unstamped` /
//! `MachineClient::unstamped`, or `build_with_source` over a fixed
//! source — never the stamping `build`.
//!
//! THE SPAWNED BINARY (car B's review, 83fc0bdf). A test that runs
//! `CARGO_BIN_EXE_boss` runs the production build, which reads
//! `BOSS_MACHINE_TOKEN_DIR` or `/etc/boss/machine-token`. Each such
//! `Command` names the directory — a scratch dir that holds no token —
//! before it runs, so the child never reads the mount either.
//!
//! WHAT IT DOES NOT HOLD, said so it is not assumed. It is a NAME scan
//! over test code: a test that reaches the shared source through a
//! production constructor of its own crate — a `Reqwest*Client::new`
//! over `http_client::base`, a service's router state built in
//! production code — is not seen. `http_client::base` called BY NAME
//! from a test is. Comments are stripped first, so a note naming a
//! constructor is not a call of it. Car B's pin,
//! `no_test_reads_the_live_machine_token`, refuses the shared source
//! and `Source::watch` named in a test; this one refuses the
//! constructors that read it for you.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::production_source::{production_text, without_comments};
use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

/// Files whose tests may build a stamping client, and why.
const EXEMPT: &[(&str, &str)] = &[
    (
        "crates/core/boss-core/src/machine_token.rs",
        "the tests OF the constructors: boss-core's own `shared()` holds no token under \
         cfg(test), which one of them proves by calling `Client::build`",
    ),
    (
        "crates/core/boss-testing/tests/no_test_builds_a_client_on_the_live_token.rs",
        "this pin, whose fixtures spell what it refuses",
    ),
];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules")
            {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The stamping constructors: boss-core's `Client::build` and
/// `BlockingClient::build`, the gateway's `MachineClient::build`, and
/// `http_client::base`, which builds a `Client` for you. `build(` alone —
/// `build_with_source(` and `builder(` are not it — and only when
/// nothing but a path separator or a non-identifier character precedes
/// the type, so `FooClient::build(` is somebody else's.
fn constructors() -> Vec<Regex> {
    [
        r"(?:^|[^A-Za-z0-9_])(?:Blocking|Machine)?Client::build\(",
        r"http_client::base\(",
    ]
    .iter()
    .map(|r| Regex::new(r).unwrap())
    .collect()
}

/// The 1-based lines of `src` that are TEST code and call a stamping
/// constructor. `whole_file_is_test` for a file under a `tests/` dir.
fn builds_in_tests(src: &str, whole_file_is_test: bool) -> Result<Vec<usize>, syn::Error> {
    let code = without_comments(src);
    let prod = if whole_file_is_test {
        String::new()
    } else {
        production_text(&code)?
    };
    let prod_lines: Vec<&str> = prod.lines().collect();
    let res = constructors();
    Ok(code
        .lines()
        .enumerate()
        // A test line is one production_text blanked.
        .filter(|(i, l)| prod_lines.get(*i).copied().unwrap_or("") != *l)
        .filter(|(_, l)| res.iter().any(|r| r.is_match(l)))
        .map(|(i, _)| i + 1)
        .collect())
}

/// The name a test spawns the CLI's production build by.
fn spawn_of_boss() -> String {
    ["env!(\"CARGO_BIN_", "EXE_boss\")"].concat()
}

/// The 1-based lines of `src` that spawn the `boss` binary and do not
/// name the token directory before the command runs: the text from the
/// spawn to the first `.output(`, `.spawn(` or `.status(` after it (or
/// to the end of the file, for a helper that hands its `Command` back)
/// must carry `TOKEN_DIR_ENV` or the variable's own name.
fn spawns_without_a_token_dir(src: &str) -> Vec<usize> {
    let code = without_comments(src);
    let spawn = spawn_of_boss();
    let dir_var = ["BOSS_MACHINE_", "TOKEN_DIR"].concat();
    let runs = Regex::new(r"\.(?:output|spawn|status)\(").unwrap();
    code.match_indices(&spawn)
        .filter(|(at, _)| {
            let rest = &code[*at..];
            let span = runs.find(rest).map_or(rest, |m| &rest[..m.start()]);
            !(span.contains("TOKEN_DIR_ENV") || span.contains(&dir_var))
        })
        .map(|(at, _)| code[..at].matches('\n').count() + 1)
        .collect()
}

struct Found {
    builds: Vec<String>,
    spawns: Vec<String>,
}

fn scan(root: &Path) -> Found {
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let mut found = Found {
        builds: Vec::new(),
        spawns: Vec::new(),
    };
    for p in &files {
        let Some(rel) = p
            .strip_prefix(root)
            .ok()
            .map(|r| r.to_string_lossy().replace('\\', "/"))
        else {
            continue;
        };
        if EXEMPT.iter().any(|(f, _)| *f == rel) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(p) else {
            continue;
        };
        let in_tests_dir = rel.split('/').any(|c| c == "tests");
        let lines = builds_in_tests(&src, in_tests_dir)
            .unwrap_or_else(|e| panic!("{rel} does not parse as Rust: {e}"));
        if !lines.is_empty() {
            found.builds.push(format!("{rel}:{lines:?}"));
        }
        let spawns = spawns_without_a_token_dir(&src);
        if !spawns.is_empty() {
            found.spawns.push(format!("{rel}:{spawns:?}"));
        }
    }
    found.builds.sort();
    found.spawns.sort();
    found
}

#[test]
fn no_test_builds_a_client_on_the_live_token() {
    let found = scan(&repo_root());
    assert!(
        found.builds.is_empty(),
        "these tests build a client that stamps the process's live machine token — read \
         from the mounted Secret — so a failing one prints the estate token into its run's \
         log (backlog 2ee29275). Build with `Client::unstamped(builder)` (or \
         `BlockingClient::unstamped`, the gateway's `MachineClient::unstamped`), or \
         `build_with_source` over `Source::fixed(..)`:\n  {}",
        found.builds.join("\n  ")
    );
}

#[test]
fn no_test_spawns_the_cli_on_the_mounted_token_dir() {
    let found = scan(&repo_root());
    assert!(
        found.spawns.is_empty(),
        "these tests run the `boss` binary — a production build that reads the machine token \
         from BOSS_MACHINE_TOKEN_DIR, else /etc/boss/machine-token — without naming a scratch \
         directory for it first (backlog 2ee29275). Set \
         `.env(boss_core::machine_token::TOKEN_DIR_ENV, <a scratch dir>)` on the Command \
         before it runs:\n  {}",
        found.spawns.join("\n  ")
    );
}

#[test]
fn the_scan_sees_a_test_build_and_not_a_production_one() {
    // Controls: a scan that saw nothing would pass the pin on any tree.
    let core = concat!("machine_token::Client::", "build(b)");
    let bare = concat!("Client::", "build(b)");
    let blocking = concat!("BlockingClient::", "build(b)");
    let gateway = concat!("MachineClient::", "build(b)");
    let base = concat!("http_client::", "base(u)");
    let file = format!(
        "fn prod() {{ let _ = {core}; let _ = {base}; }}\n\
         #[cfg(test)]\n\
         mod tests {{\n\
         \x20   fn a() {{ let _ = {core}; }}\n\
         \x20   fn b() {{ let _ = {bare}; let _ = {blocking}; }}\n\
         \x20   fn c() {{ let _ = {gateway}; }}\n\
         \x20   fn d() {{ let _ = {base}; }}\n\
         \x20   // {gateway} in a comment is not a call\n\
         \x20   fn e() {{ let _ = Client::unstamped(b); let _ = Client::build_with_source(b, s); }}\n\
         \x20   fn f() {{ let _ = reqwest::Client::builder(); let _ = FooClient::build(x); }}\n\
         }}\n"
    );
    assert_eq!(builds_in_tests(&file, false).unwrap(), vec![4, 5, 6, 7]);
    // Under tests/, every line is test code.
    assert_eq!(builds_in_tests(&file, true).unwrap(), vec![1, 4, 5, 6, 7]);
}

#[test]
fn the_spawn_scan_sees_a_command_without_the_dir_and_not_one_with_it() {
    let spawn = spawn_of_boss();
    let dir_var = ["BOSS_MACHINE_", "TOKEN_DIR"].concat();
    let file = format!(
        "fn bare() {{\n\
         \x20   let out = Command::new({spawn}).args([\"x\"]).output().unwrap();\n\
         }}\n\
         fn named() {{\n\
         \x20   let mut c = Command::new({spawn});\n\
         \x20   c.env(boss_core::machine_token::TOKEN_DIR_ENV, &d);\n\
         \x20   c.output().unwrap();\n\
         }}\n\
         fn spelled() {{\n\
         \x20   Command::new({spawn}).env(\"{dir_var}\", &d).status().unwrap();\n\
         }}\n\
         fn named_after_it_ran() {{\n\
         \x20   let mut c = Command::new({spawn});\n\
         \x20   c.output().unwrap();\n\
         \x20   c.env(\"{dir_var}\", &d);\n\
         }}\n\
         fn handed_back() -> Command {{\n\
         \x20   let mut c = Command::new({spawn});\n\
         \x20   c\n\
         }}\n"
    );
    assert_eq!(spawns_without_a_token_dir(&file), vec![2, 13, 18]);
}
