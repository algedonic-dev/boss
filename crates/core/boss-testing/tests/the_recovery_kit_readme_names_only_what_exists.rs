//! The recovery kit's README is read at the worst moment there is: during
//! a recovery, off a stick, by the one person holding it. Every pointer it
//! carries is followed then, and a pointer that leads nowhere is found then.
//!
//! Backlog e81b4ecb: the README sent its reader to
//! `docs/runbooks/access-recovery.md` for the Google, GitHub and
//! Cloudflare account map — a file train #808 had deleted. Nothing held
//! the README's paths to the tree, so a stick cut that day carried the
//! dead pointer. Two pins, so it cannot rot again (CLAUDE.md §9a):
//!
//! * every repository path the README names exists in the tree (a
//!   `<placeholder>` segment is judged by the directory before it);
//! * every recovery-sheet road the README names by title is a road the
//!   sheet's source (`infra/recovery/re-entry.toml`) declares, so the
//!   stick and the paper agree.

use boss_testing::repo_root;

const README: &str = "infra/forge/recovery-kit-README.txt";
const SHEET: &str = "infra/recovery/re-entry.toml";

/// The README with its hard wraps undone: a path broken after a `/` at
/// the end of a line is joined to the next line's first word, and every
/// other line break is a space — the dead pointer this pins was itself
/// wrapped (`docs/runbooks/` then `access-recovery.md`), and a line-based
/// read would have judged the directory, which exists, and passed.
fn unwrapped() -> String {
    let text = std::fs::read_to_string(repo_root().join(README)).expect("the README");
    let joined = regex::Regex::new(r"/\n\s*")
        .expect("regex")
        .replace_all(&text, "/");
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn named_paths(text: &str) -> Vec<String> {
    let re = regex::Regex::new(
        r"(?:^|[\s(`'])((?:infra|docs|crates|apps|examples|libs|schema)/[A-Za-z0-9_./<>-]+)",
    )
    .expect("regex");
    re.captures_iter(text)
        .map(|c| c[1].trim_end_matches(['.', ',', ':', ';']).to_string())
        .collect()
}

#[test]
fn every_repository_path_the_readme_names_exists() {
    let paths = named_paths(&unwrapped());
    assert!(
        paths.len() >= 5,
        "the README names {} repository paths — the reader found too few to judge: {paths:?}",
        paths.len()
    );
    let missing: Vec<&String> = paths
        .iter()
        .filter(|p| {
            // `infra/cluster/talos/patches/<node>.yaml`: the placeholder is
            // the reader's to fill, the directory before it is the tree's.
            let judged = match p.find('<') {
                Some(i) => &p[..i],
                None => p.as_str(),
            };
            !repo_root().join(judged).exists()
        })
        .collect();
    assert!(
        missing.is_empty(),
        "{README} names repository paths the tree does not hold — a recovery that follows them finds nothing: {missing:?}"
    );
}

#[test]
fn every_sheet_road_the_readme_names_is_a_road_on_the_sheet() {
    let text = unwrapped();
    let sheet = std::fs::read_to_string(repo_root().join(SHEET)).expect("the sheet's source");
    let re = regex::Regex::new(r#"road "([^"]+)""#).expect("regex");
    let titles: Vec<String> = re.captures_iter(&text).map(|c| c[1].to_string()).collect();
    assert!(
        !titles.is_empty(),
        "{README} names no recovery-sheet road — the account map lives on the sheet (backlog e81b4ecb)"
    );
    let missing: Vec<&String> = titles
        .iter()
        .filter(|t| !sheet.contains(&format!("title = \"{t}\"")))
        .collect();
    assert!(
        missing.is_empty(),
        "{README} names sheet roads {SHEET} does not declare — the stick and the paper disagree: {missing:?}"
    );
}
