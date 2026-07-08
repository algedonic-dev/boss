//! Corpus gate: every design doc in the repo must present sensibly
//! through the review surface — parse to a titled, rendered doc with
//! explicit `### Qn:` anchors (never the silent positional fallback
//! CLAUDE.md warns about, where authored questions simply don't show
//! up in the UI).
//!
//! This is the CI teeth behind the authoring convention: a doc that
//! merges here will present correctly at /system/design and in the
//! review-design step plugin, or the PR fails with the reason.

use std::fs;
use std::path::PathBuf;

use boss_docs::parser::parse_doc;

fn design_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("docs/design")
}

#[test]
fn every_design_doc_presents_sensibly() {
    let dir = design_dir();
    let mut checked = 0;
    for entry in fs::read_dir(&dir).expect("docs/design exists") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let markdown = fs::read_to_string(&path).expect("doc reads");
        let rel = format!("docs/design/{name}");
        let parsed = parse_doc(&rel, &markdown);

        assert!(
            !parsed.title.is_empty(),
            "{name}: no `# H1` title — the docs list would show an empty row"
        );
        assert!(
            !parsed.content_html.trim().is_empty(),
            "{name}: rendered to empty HTML"
        );
        assert!(
            markdown.lines().take(30).any(|l| {
                let t = l.trim_start().trim_start_matches("- ");
                t.starts_with("**Status**") || t.starts_with("**Status:**")
            }),
            "{name}: no `**Status**:` line in the first 30 lines — the doc \
             defaults to in-review and reads as under active discussion \
             (declare `living` for a settled reference)"
        );
        if parsed.status.forbids_open_questions() {
            assert!(
                parsed.questions.is_empty(),
                "{name}: status `{}` asserts no open discussion but the doc \
                 has {} open question(s) — flip to `reopened` first",
                parsed.status.as_str(),
                parsed.questions.len()
            );
        }
        assert!(
            !parsed.used_fallback_anchors,
            "{name}: open questions use positional fallback anchors — author them as \
             `### Qn: <title>` subheadings (CLAUDE.md §Design docs) or reordering \
             breaks recorded decisions"
        );

        // A `## Open questions` section that *contains H3 headings* must
        // yield parsed questions — otherwise the headings are malformed
        // (`### Q1 —` instead of `### Q1: …`) and the review UI would
        // silently show "no open questions" for a doc that has them.
        if let Some(section) = open_questions_section(&markdown) {
            let h3_count = section
                .lines()
                .filter(|l| l.trim_start().starts_with("### "))
                .count();
            assert_eq!(
                parsed.questions.len(),
                h3_count,
                "{name}: `## Open questions` has {h3_count} `###` headings but only \
                 {} parsed as questions — malformed `### Qn:` anchor(s)",
                parsed.questions.len()
            );
        }
        checked += 1;
    }
    // The corpus ships multiple living references; zero means the walk
    // silently matched nothing (wrong path, renamed dir).
    assert!(
        checked >= 5,
        "expected ≥5 design docs under docs/design, found {checked}"
    );
}

/// The text between `## Open questions` and the next `## ` heading (or
/// EOF). Mirrors the section walk the parser does, shaped for a lint.
fn open_questions_section(markdown: &str) -> Option<String> {
    let mut in_section = false;
    let mut out = String::new();
    for line in markdown.lines() {
        let is_h2 = line.starts_with("## ");
        if in_section && is_h2 {
            break;
        }
        if is_h2
            && line
                .trim_start_matches("## ")
                .trim()
                .eq_ignore_ascii_case("open questions")
        {
            in_section = true;
            continue;
        }
        if in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    in_section.then_some(out)
}
