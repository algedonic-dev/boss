//! A test's dark address is HELD, never bound and released.
//!
//! WHY (review 465e723f F3, backlog c0259ecc). The way to name an
//! address nothing answers on was to bind `127.0.0.1:0`, read the port,
//! and drop the listener. From the drop on, the port is free, and any
//! `bind(0)` or outbound connect in any process on the host may be given
//! it — then the "dark" address answers, or a server meant for a
//! neighbour is reached. `boss_testing::dark_port` (backlog ec131700)
//! records that race, 3 red in 90 runs in one binary, and holds the port
//! bound and unlistened instead. The gate now runs four test binaries at
//! once (infra/cargo-test-wide.sh, backlog 2d234222), which multiplies
//! the processes calling `bind(0)` beside each released port, so the
//! twelve sites the review named were moved and this pin keeps the class
//! closed rather than the instances.
//!
//! THE SHAPES REFUSED, each a listener on `127.0.0.1:0` that is gone
//! before anything listens through it:
//!   * `drop(<it>)` within a few lines, with no serve/accept/spawn between;
//!   * the bind and `.local_addr()` in ONE statement — the listener is a
//!     temporary, dropped at its `;`;
//!   * the bind as the first line of a `let x = { … };` block that closes
//!     without serving — the listener dies with the block.
//!
//! A port handed to a CHILD process to bind (the reverse race) is not a
//! dark address and `dark_port` is not its answer: the child binds `0`
//! itself and reports the port it got.
//!
//! tree-wide pin — it scans every crate's Rust sources, which no
//! changed-file map attributes to boss-testing, so a scoped gate must
//! run it for a car that adds a released port in any crate.

use boss_testing::repo_root;
use std::fs;
use std::path::{Path, PathBuf};

const BIND_ZERO: &str = "bind(\"127.0.0.1:0\")";
/// Words that mean the listener is put to use before it goes.
const USED: [&str; 5] = ["serve", "accept", "spawn", "incoming", "into_std"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        if p.is_dir() {
            if name != "target" && name != "node_modules" {
                rust_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Each released dark address in `src`, as (1-based line, shape).
fn released(src: &str) -> Vec<(usize, &'static str)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !line.contains(BIND_ZERO) {
            continue;
        }
        // The statement the bind opens, through its `;`.
        let end = (i..lines.len().min(i + 8))
            .find(|&k| lines[k].contains(';'))
            .unwrap_or(i);
        let statement = lines[i..=end].join(" ");
        if statement.contains(".local_addr()") {
            found.push((
                i + 1,
                "bound and read in one statement: the listener is a temporary",
            ));
            continue;
        }
        let used_before = |upto: usize| {
            lines[i..upto]
                .iter()
                .any(|l| USED.iter().any(|w| l.contains(w)))
        };
        let name = line
            .split("let ")
            .nth(1)
            .map(|rest| {
                rest.trim_start_matches("mut ")
                    .split([' ', '=', ':'])
                    .next()
                    .unwrap_or("")
            })
            .unwrap_or("");
        if !name.is_empty() {
            let drop = format!("drop({name})");
            if let Some(k) = (end + 1..lines.len().min(end + 6)).find(|&k| lines[k].contains(&drop))
                && !used_before(k)
            {
                found.push((i + 1, "bound, read, and dropped"));
                continue;
            }
        }
        if i > 0
            && lines[i - 1].trim_end().ends_with("= {")
            && let Some(k) = (end + 1..lines.len().min(end + 5)).find(|&k| {
                let t = lines[k].trim();
                t == "};" || t == "}"
            })
            && !used_before(k)
        {
            found.push((i + 1, "bound inside a block that closes without serving"));
        }
    }
    found
}

#[test]
fn no_test_makes_a_dark_address_by_releasing_a_bound_port() {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    files.sort();
    assert!(
        files.len() > 500,
        "the scan found {} Rust files under crates/ — too few to be this tree, so the walk is broken",
        files.len()
    );
    let mut hits = Vec::new();
    for f in &files {
        let Ok(src) = fs::read_to_string(f) else {
            continue;
        };
        for (line, shape) in released(&src) {
            hits.push(format!(
                "  {}:{line} — {shape}",
                f.strip_prefix(&root).unwrap_or(f).display()
            ));
        }
    }
    assert!(
        hits.is_empty(),
        "{} dark address(es) made by releasing a bound port — between the release and the \
         connect, any bind(0) on the host may be handed it. Hold one with \
         `let dark = boss_testing::dark_port();` and use `dark.base` / `dark.addr` while \
         `dark` is in scope (backlog ec131700; review 465e723f F3):\n{}",
        hits.len(),
        hits.join("\n")
    );
}

/// The scanner, on each shape it refuses and the ones it must not.
#[test]
fn the_scanner_finds_each_released_shape_and_passes_a_held_or_served_one() {
    let dropped = "let listener = tokio::net::TcpListener::bind(\"127.0.0.1:0\").await.unwrap();\n\
                   let dark = format!(\"http://{}\", listener.local_addr().unwrap());\n\
                   drop(listener);\n";
    let temporary = "let port = std::net::TcpListener::bind(\"127.0.0.1:0\")\n\
                     .expect(\"bind\")\n.local_addr()\n.expect(\"addr\")\n.port();\n";
    let block = "let closed = {\n\
                 let l = std::net::TcpListener::bind(\"127.0.0.1:0\").unwrap();\n\
                 l.local_addr().unwrap()\n\
                 };\n";
    let served = "let listener = tokio::net::TcpListener::bind(\"127.0.0.1:0\").await.unwrap();\n\
                  let addr = listener.local_addr().unwrap();\n\
                  tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });\n";
    let held = "let dark = boss_testing::dark_port();\nlet url = dark.base.clone();\n";
    assert_eq!(released(dropped).len(), 1, "drop");
    assert_eq!(released(temporary).len(), 1, "temporary");
    assert_eq!(released(block).len(), 1, "block");
    assert!(
        released(served).is_empty(),
        "a served listener is not released"
    );
    assert!(released(held).is_empty(), "a held dark port is the answer");
}
