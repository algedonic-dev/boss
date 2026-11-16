//! tree-wide pin — a new host_check caller must declare what it guards.
//! The runtime guard counts executed declarations; this independent inventory
//! catches missing declarations before a new caller reaches a managed host.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

fn audit(body: &str) -> Result<Vec<usize>, String> {
    if !body.contains("host_check") {
        return Ok(Vec::new());
    }
    let heredoc = regex::Regex::new(r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#).unwrap();
    let word = regex::Regex::new(r"\bhost_check\b").unwrap();
    let mut delimiter: Option<String> = None;
    let mut declarations = 0;
    let mut calls = Vec::new();
    for (index, line) in body.lines().enumerate() {
        if let Some(end) = &delimiter {
            if line.trim_start_matches('\t') == end {
                delimiter = None;
            }
            continue;
        }
        let code = line.trim_start();
        if code.starts_with('#') {
            continue;
        }
        let command = code.split_whitespace().next().unwrap_or_default();
        if matches!(command, "host_seam" | "host_path" | "host_command") {
            declarations += 1;
        }
        if command == "host_check" {
            if declarations == 0 {
                return Err(format!(
                    "line {}: host_check has no preceding host declaration",
                    index + 1
                ));
            }
            calls.push(index + 1);
        } else if word.is_match(code) {
            return Err(format!(
                "line {}: unrecognised host_check call shape; audit it explicitly",
                index + 1
            ));
        }
        let documents: Vec<_> = heredoc.captures_iter(code).collect();
        if documents.len() > 1 {
            return Err(format!("line {}: ambiguous multiple heredocs", index + 1));
        }
        if let Some(capture) = documents.first() {
            delimiter = Some(capture[1].to_owned());
        }
    }
    if delimiter.is_some() {
        return Err("unclosed heredoc; cannot establish caller declarations".to_owned());
    }
    Ok(calls)
}

fn scripts(dir: &Path, result: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            scripts(&entry.path(), result);
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "sh")
        {
            assert!(
                kind.is_file(),
                "shell source must be a regular file: {:?}",
                entry.path()
            );
            result.push(entry.path());
        }
    }
}

#[test]
fn every_host_check_caller_declares_what_it_guards_before_the_call() {
    let infra = repo_root().join("infra");
    let mut paths = Vec::new();
    scripts(&infra, &mut paths);
    paths.sort();
    let mut callers = Vec::new();
    for path in paths {
        if path == infra.join("lib/host-check.sh") {
            continue;
        }
        let body = std::fs::read_to_string(&path).unwrap();
        let calls = audit(&body).unwrap_or_else(|why| panic!("{}: {why}", path.display()));
        for line in calls {
            callers.push(format!(
                "{}:{line}",
                path.strip_prefix(&infra).unwrap().display()
            ));
        }
    }
    assert!(!callers.is_empty(), "an empty inventory is not proof");
    println!(
        "host_check callers: {}\n{}",
        callers.len(),
        callers.join("\n")
    );
}

#[test]
fn the_inventory_rejects_a_missing_late_or_only_documented_declaration() {
    let valid = "host_seam TARGET /real\nhost_check forge publisher\n";
    assert_eq!(audit(valid).unwrap(), [2]);
    assert!(audit(&valid.replace("host_seam TARGET /real\n", "")).is_err());
    assert!(audit("host_check forge publisher\nhost_seam TARGET /real\n").is_err());
    assert!(audit("# host_seam TARGET /real\nhost_check forge publisher\n").is_err());
    assert!(
        audit("cat <<'DOC'\nhost_seam TARGET /real\nDOC\nhost_check forge publisher\n").is_err()
    );
    assert!(audit("if host_check forge publisher; then :; fi\n").is_err());
    for declaration in [
        "host_seam TARGET /real",
        "host_path TARGET /else /real",
        "host_command useradd",
    ] {
        assert!(audit(&format!("{declaration}\nhost_check forge publisher\n")).is_ok());
    }
}
