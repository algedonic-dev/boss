//! Markdown parser for design docs.
//!
//! Extracts:
//! - Title (first H1)
//! - Status (from the `**Status**: ...` line near the top)
//! - Open questions from the `## Open questions` section
//! - Pre-rendered HTML of the full document
//! - Word count
//!
//! Stable question anchors use the convention `### Q1:`, `### Q2:`, etc.
//! Numbered lists without anchors fall back to positional ids
//! (`{doc_slug}-open-0`, `-open-1`, ...). The fallback is marked
//! unstable in the returned `ParsedDoc` so callers can warn.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::types::{DesignQuestion, DocStatus};

/// Output of parsing a single markdown file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDoc {
    pub title: String,
    pub status: DocStatus,
    pub word_count: i32,
    pub content_html: String,
    pub questions: Vec<DesignQuestion>,
    /// True when the parser fell back to positional ids for any
    /// question (i.e. the doc has numbered-list open questions
    /// instead of explicit `### Qn:` anchors).
    pub used_fallback_anchors: bool,
    /// Titles of any `### Qn:` headings anywhere in the doc whose
    /// heading text doesn't contain `(resolved)`. Used by the
    /// reindex validator to reject a Shipped/Superseded doc that
    /// still has live questions — the author has to either mark
    /// them resolved or transition the doc to Reopened.
    pub unresolved_questions: Vec<String>,
}

/// Parse a design doc. `doc_path` is used to stamp question ids.
pub fn parse_doc(doc_path: &str, markdown: &str) -> ParsedDoc {
    let title = extract_title(markdown).unwrap_or_else(|| "Untitled".to_string());
    let status = extract_status(markdown);
    let word_count = markdown.split_whitespace().count() as i32;
    let content_html = render_html(markdown);
    let (questions, used_fallback_anchors) = extract_open_questions(doc_path, markdown);
    let unresolved_questions = extract_unresolved_question_titles(markdown);

    ParsedDoc {
        title,
        status,
        word_count,
        content_html,
        questions,
        used_fallback_anchors,
        unresolved_questions,
    }
}

/// Walk every H3 heading in the document and return the titles of
/// those shaped like `Qn: ...` that do NOT contain `(resolved)`.
///
/// Looks doc-wide, not just inside `## Open questions` — authors
/// sometimes put questions next to the proposal they belong to, and
/// we still want to catch them. Detection is by heading text only;
/// the tracker elsewhere decides where a question lives on screen.
pub fn extract_unresolved_question_titles(markdown: &str) -> Vec<String> {
    let opts = Options::empty();
    let parser = Parser::new_ext(markdown, opts);
    let events: Vec<Event> = parser.collect();

    let mut unresolved = Vec::new();
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Heading {
            level: HeadingLevel::H3,
            ..
        }) = &events[i]
        {
            let mut heading = String::new();
            let mut j = i + 1;
            while j < events.len() {
                match &events[j] {
                    Event::Text(t) => heading.push_str(t),
                    Event::Code(c) => heading.push_str(c),
                    Event::End(TagEnd::Heading(HeadingLevel::H3)) => break,
                    _ => {}
                }
                j += 1;
            }
            if parse_h3_anchor(&heading).is_some() && !heading.to_lowercase().contains("(resolved)")
            {
                unresolved.push(heading.trim().to_string());
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    unresolved
}

/// Markdown → HTML with the doc pipeline's options — one renderer for
/// whole docs and per-question bodies, so the two can't drift.
pub(crate) fn render_html(markdown: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(markdown, opts);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

fn extract_title(markdown: &str) -> Option<String> {
    let parser = Parser::new(markdown);
    let mut in_h1 = false;
    let mut title = String::new();
    for event in parser {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => in_h1 = true,
            Event::End(TagEnd::Heading(HeadingLevel::H1)) => {
                if !title.is_empty() {
                    return Some(title);
                }
                in_h1 = false;
            }
            Event::Text(t) if in_h1 => title.push_str(&t),
            Event::Code(c) if in_h1 => title.push_str(&c),
            _ => {}
        }
    }
    None
}

/// Pull the status from a `**Status**: ...` line. Matches the
/// convention the existing design docs already use at the top of the
/// document, immediately after the H1 title.
fn extract_status(markdown: &str) -> DocStatus {
    for line in markdown.lines().take(30) {
        let lower = line.to_lowercase();
        // Strip common leading markers so we match both bullet-list
        // and prose-line forms of the status field.
        let stripped = lower
            .trim_start_matches("- ")
            .trim_start_matches("* ")
            .trim_start();
        if let Some(rest) = stripped.strip_prefix("**status**:") {
            return DocStatus::from_status_line(rest.trim());
        }
        if let Some(rest) = stripped.strip_prefix("status:") {
            return DocStatus::from_status_line(rest.trim());
        }
    }
    DocStatus::InReview
}

/// Compute a URL-safe slug for fallback anchor ids.
fn slug_for(doc_path: &str) -> String {
    let stem = std::path::Path::new(doc_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(doc_path);
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Try to extract a stable anchor from an H3 heading like "Q1: foo".
/// Returns the anchor (`Q1`) and the remaining title text.
fn parse_h3_anchor(heading: &str) -> Option<(String, String)> {
    let trimmed = heading.trim_start();
    // Accept "Q<number>:" or "D<number>:" (we only care about Q here,
    // but the same shape works for future decision anchors).
    let chars = trimmed.chars().collect::<Vec<_>>();
    if chars.len() < 3 {
        return None;
    }
    let first = chars[0];
    if !(first == 'Q' || first == 'q') {
        return None;
    }
    let mut end = 1;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    if end == 1 {
        return None;
    }
    if end >= chars.len() || chars[end] != ':' {
        return None;
    }
    let anchor = format!("Q{}", chars[1..end].iter().collect::<String>());
    let remainder: String = chars[end + 1..]
        .iter()
        .collect::<String>()
        .trim()
        .to_string();
    Some((anchor, remainder))
}

/// Walk the markdown event stream to find the `## Open questions`
/// section and extract every question in it.
///
/// Two formats are supported:
///
/// 1. **Explicit anchors** (preferred): each question is an H3 heading
///    shaped like `### Q1: <title>` inside the section. The parser
///    captures everything between the heading and the next H3 or
///    the end of the section.
/// 2. **Numbered list** (fallback): the first numbered ordered list
///    inside the section is treated as the question list, each item
///    becomes one question, and anchors get positional ids
///    `{slug}-open-0`, `-open-1`, ...
///
/// If both formats are present, explicit anchors win.
fn extract_open_questions(doc_path: &str, markdown: &str) -> (Vec<DesignQuestion>, bool) {
    let opts = Options::empty();
    let parser = Parser::new_ext(markdown, opts);

    let events: Vec<Event> = parser.collect();

    // Locate the start of the `## Open questions` section and its end
    // (next H2 or EOF).
    let Some(section_start) = find_section_start(&events, "open questions") else {
        return (Vec::new(), false);
    };
    let section_end = find_next_section_end(&events, section_start);

    let section = &events[section_start..section_end];

    // First try explicit H3 anchors.
    let mut questions = extract_questions_via_anchors(doc_path, section);
    if !questions.is_empty() {
        return (questions, false);
    }

    // Fall back to numbered list items.
    questions = extract_questions_via_list(doc_path, section);
    let used_fallback = !questions.is_empty();
    (questions, used_fallback)
}

/// Returns the index of the Event immediately after the
/// `## <target>` heading, or None if not found.
fn find_section_start(events: &[Event], target: &str) -> Option<usize> {
    let target = target.to_lowercase();
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Heading {
            level: HeadingLevel::H2,
            ..
        }) = &events[i]
        {
            // Collect heading text until End.
            let mut heading = String::new();
            let mut j = i + 1;
            while j < events.len() {
                match &events[j] {
                    Event::Text(t) => heading.push_str(t),
                    Event::Code(c) => heading.push_str(c),
                    Event::End(TagEnd::Heading(HeadingLevel::H2)) => break,
                    _ => {}
                }
                j += 1;
            }
            if heading.trim().to_lowercase() == target {
                return Some(j + 1);
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    None
}

/// Returns the index where the section ends (next H2 start, or len).
fn find_next_section_end(events: &[Event], start: usize) -> usize {
    let mut i = start;
    while i < events.len() {
        if matches!(
            &events[i],
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            })
        ) {
            return i;
        }
        i += 1;
    }
    events.len()
}

/// Look for `### Q<n>: <title>` headings inside the slice and build
/// one question per heading. Body = events between the heading end
/// and the next H3 / end of slice.
fn extract_questions_via_anchors(doc_path: &str, section: &[Event]) -> Vec<DesignQuestion> {
    let mut questions = Vec::new();

    // Collect heading positions.
    let mut heading_starts: Vec<(usize, String, String)> = Vec::new();
    let mut i = 0;
    while i < section.len() {
        if let Event::Start(Tag::Heading {
            level: HeadingLevel::H3,
            ..
        }) = &section[i]
        {
            // Collect heading text.
            let mut heading = String::new();
            let mut j = i + 1;
            while j < section.len() {
                match &section[j] {
                    Event::Text(t) => heading.push_str(t),
                    Event::Code(c) => heading.push_str(c),
                    Event::End(TagEnd::Heading(HeadingLevel::H3)) => break,
                    _ => {}
                }
                j += 1;
            }
            if let Some((anchor, title)) = parse_h3_anchor(&heading) {
                heading_starts.push((j + 1, anchor, title));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }

    if heading_starts.is_empty() {
        return questions;
    }

    // Now slice bodies between headings.
    for (ordinal, (start_idx, anchor, title)) in heading_starts.iter().enumerate() {
        let end_idx = heading_starts
            .get(ordinal + 1)
            .map(|(next_start, _, _)| find_h3_start_before(section, *next_start))
            .unwrap_or(section.len());
        let body_events = &section[*start_idx..end_idx];
        let body_md = events_to_markdown(body_events);
        let proposal = extract_proposal(&body_md);
        questions.push(DesignQuestion {
            id: format!("{doc_path}#{anchor}"),
            doc_path: doc_path.to_string(),
            anchor: anchor.clone(),
            ordinal: ordinal as i32,
            title: title.clone(),
            body_md: body_md.trim().to_string(),
            proposal,
            context_md: None,
        });
    }

    questions
}

/// Given an index pointing at the content after an H3 end, walk
/// backwards to find the matching H3 start.
fn find_h3_start_before(section: &[Event], idx: usize) -> usize {
    let mut i = idx;
    while i > 0 {
        i -= 1;
        if matches!(
            &section[i],
            Event::Start(Tag::Heading {
                level: HeadingLevel::H3,
                ..
            })
        ) {
            return i;
        }
    }
    0
}

/// Extract questions from the first ordered list in the section.
fn extract_questions_via_list(doc_path: &str, section: &[Event]) -> Vec<DesignQuestion> {
    let mut questions = Vec::new();
    let slug = slug_for(doc_path);

    // Find the first ordered-list tag.
    let Some(list_start) = section
        .iter()
        .position(|e| matches!(e, Event::Start(Tag::List(Some(_)))))
    else {
        return questions;
    };

    // Find the matching list end.
    let mut depth = 0i32;
    let mut list_end = list_start;
    for (offset, e) in section[list_start..].iter().enumerate() {
        match e {
            Event::Start(Tag::List(Some(_))) => depth += 1,
            Event::End(TagEnd::List(true)) => {
                depth -= 1;
                if depth == 0 {
                    list_end = list_start + offset;
                    break;
                }
            }
            _ => {}
        }
    }

    // Walk items at depth 1.
    let mut ordinal = 0i32;
    let mut i = list_start + 1;
    let mut item_start: Option<usize> = None;
    let mut item_depth = 0i32;
    while i <= list_end {
        match &section[i] {
            Event::Start(Tag::Item) => {
                if item_depth == 0 {
                    item_start = Some(i + 1);
                }
                item_depth += 1;
            }
            Event::End(TagEnd::Item) => {
                item_depth -= 1;
                if item_depth == 0 {
                    if let Some(start) = item_start {
                        let item_events = &section[start..i];
                        let body_md = events_to_markdown(item_events).trim().to_string();
                        let anchor = format!("{slug}-open-{ordinal}");
                        let title = first_sentence(&body_md);
                        let proposal = extract_proposal(&body_md);
                        questions.push(DesignQuestion {
                            id: format!("{doc_path}#{anchor}"),
                            doc_path: doc_path.to_string(),
                            anchor,
                            ordinal,
                            title,
                            body_md,
                            proposal,
                            context_md: None,
                        });
                        ordinal += 1;
                    }
                    item_start = None;
                }
            }
            Event::Start(Tag::List(Some(_))) => {}
            _ => {}
        }
        i += 1;
    }

    questions
}

/// Best-effort "turn events back into markdown" for body display.
/// This is NOT a faithful roundtrip — we only care about reading the
/// text + basic formatting, not preserving every construct.
fn events_to_markdown(events: &[Event]) -> String {
    let mut out = String::new();
    for e in events {
        match e {
            Event::Text(t) => out.push_str(t),
            Event::Code(c) => {
                out.push('`');
                out.push_str(c);
                out.push('`');
            }
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("\n\n"),
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),
            Event::Start(Tag::Emphasis) | Event::End(TagEnd::Emphasis) => out.push('*'),
            Event::Start(Tag::Strong) | Event::End(TagEnd::Strong) => out.push_str("**"),
            Event::Start(Tag::List(Some(_))) => {}
            Event::Start(Tag::List(None)) => {}
            Event::End(TagEnd::List(_)) => {}
            Event::Start(Tag::Item) => out.push_str("- "),
            Event::End(TagEnd::Item) => out.push('\n'),
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    _ => String::new(),
                };
                out.push_str("```");
                out.push_str(&lang);
                out.push('\n');
            }
            Event::End(TagEnd::CodeBlock) => {
                out.push_str("```\n");
            }
            _ => {}
        }
    }
    out
}

/// Look for a `**Proposal**: ...` line in the question body and
/// return its text. Multi-sentence proposals collapse onto one line.
fn extract_proposal(body: &str) -> Option<String> {
    // Scan for a bolded "Proposal" anywhere — the common shapes are
    // `**Proposal**: foo.`, or prose like `... **Proposal**: foo.`.
    let needle = "**Proposal**";
    let idx = body.find(needle)?;
    let after = &body[idx + needle.len()..];
    let after = after.trim_start_matches(':').trim_start();
    // Grab up to the next double-newline or end.
    let end = after.find("\n\n").unwrap_or(after.len());
    let text = after[..end].replace('\n', " ").trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Title extractor for numbered-list open questions. Handles two
/// shapes, matching the flush worker's `extract_item_title`:
///
/// 1. `**Bolded question?** body text` → everything between the
///    opening `**` and the matching closing `**`, ignoring
///    anything that looks like a delimiter inside inline code.
/// 2. `Plain prose question.` → everything up to the first
///    sentence terminator that's NOT inside an inline-code span.
///
/// The earlier v1 version stopped at the first `.` anywhere in
/// the body, which truncated questions containing inline code
/// like `` `schema.sql` `` or URLs mid-sentence. Regression test:
/// `parser::tests::extracts_multi_clause_questions_with_inline_code`.
fn first_sentence(body: &str) -> String {
    let collapsed = body.replace('\n', " ");
    let trimmed = collapsed.trim();

    if let Some(rest) = trimmed.strip_prefix("**")
        && let Some(end) = find_closing_bold(rest)
    {
        return rest[..end].trim().to_string();
    }

    first_sentence_outside_code(trimmed)
}

/// Scan forward through `s` looking for the closing `**` of a
/// bolded span. Skips delimiters that appear inside inline code.
fn find_closing_bold(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i + 1 < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if !in_code && c == b'*' && bytes[i + 1] == b'*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Walk forward looking for `.`, `?`, or `!` outside of any
/// inline code span.
fn first_sentence_outside_code(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if !in_code && (c == b'.' || c == b'?' || c == b'!') {
            return s[..i].trim().to_string();
        }
        i += 1;
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_extraction() {
        let md = "# Design: Simulator UI\n\nSome body.";
        let parsed = parse_doc("docs/design/sim.md", md);
        assert_eq!(parsed.title, "Design: Simulator UI");
    }

    #[test]
    fn status_extraction_from_bold_line() {
        let md = "# Foo\n\n**Status**: in-review\n";
        let parsed = parse_doc("docs/design/foo.md", md);
        assert_eq!(parsed.status, DocStatus::InReview);
    }

    #[test]
    fn status_extraction_draft() {
        let md = "# Foo\n\n**Status**: draft, pre-review\n";
        let parsed = parse_doc("docs/design/foo.md", md);
        assert_eq!(parsed.status, DocStatus::Draft);
    }

    #[test]
    fn status_extraction_design_pre_implementation_is_in_review() {
        let md = "# Foo\n\n**Status**: design, pre-implementation.\n";
        let parsed = parse_doc("docs/design/foo.md", md);
        // The existing docs use this phrase for "approved design but not yet built."
        // We map it to in-review since questions may still be open.
        assert_eq!(parsed.status, DocStatus::InReview);
    }

    #[test]
    fn extracts_questions_with_explicit_anchors() {
        let md = r#"# Test

**Status**: in-review.

## Open questions

Each has a proposal attached.

### Q1: First thing?

Some body text explaining Q1.

**Proposal**: do the first thing.

### Q2: Second thing?

Body for Q2.

**Proposal**: pick option B.

## Next section

Irrelevant content.
"#;
        let parsed = parse_doc("docs/design/test.md", md);
        assert!(!parsed.used_fallback_anchors);
        assert_eq!(parsed.questions.len(), 2);
        assert_eq!(parsed.questions[0].anchor, "Q1");
        assert_eq!(parsed.questions[0].title, "First thing?");
        assert_eq!(
            parsed.questions[0].proposal.as_deref(),
            Some("do the first thing.")
        );
        assert_eq!(parsed.questions[0].id, "docs/design/test.md#Q1");
        assert_eq!(parsed.questions[1].anchor, "Q2");
        assert_eq!(
            parsed.questions[1].proposal.as_deref(),
            Some("pick option B.")
        );
    }

    #[test]
    fn extracts_questions_from_numbered_list_fallback() {
        let md = r#"# Test

**Status**: in-review

## Open questions

These are unresolved.

1. **Where does X live?** Some discussion. **Proposal**: in a new crate.
2. **Should Y be sync or async?** More discussion. **Proposal**: async.

## Next section
"#;
        let parsed = parse_doc("docs/design/test.md", md);
        assert!(parsed.used_fallback_anchors);
        assert_eq!(parsed.questions.len(), 2);
        assert_eq!(parsed.questions[0].anchor, "test-open-0");
        assert_eq!(parsed.questions[0].title, "Where does X live?");
        assert_eq!(parsed.questions[1].anchor, "test-open-1");
        assert_eq!(parsed.questions[1].title, "Should Y be sync or async?");
        assert!(
            parsed.questions[0].proposal.as_deref() == Some("in a new crate.")
                || parsed.questions[0].proposal.as_deref() == Some("in a new crate"),
            "got proposal: {:?}",
            parsed.questions[0].proposal
        );
    }

    /// Regression: the v1 parser truncated titles on the first
    /// `.` anywhere, which broke questions containing inline code
    /// like `` `schema.sql` `` or file paths like `/api/sim`.
    /// Dogfood surfaced this against an early-2026 design doc with
    /// inline-code questions (since resolved and folded into
    /// `docs/architecture-decisions.md`).
    #[test]
    fn extracts_multi_clause_questions_with_inline_code() {
        let md = r#"# Test

**Status**: in-review

## Open questions

1. **What happens if the scratch DB is out of sync with `schema.sql` (e.g. after pulling new migrations)?** The reset endpoint should always re-apply. **Proposal**: document this.
2. **Who can hit `/api/sim`?** Sim UI is an operator tool. **Proposal**: v1 = any authenticated session.
3. **Does the scratch stack run all 7 services, or only the ones the sim writes to?** The sim writes to fleet, people, commerce, inventory, shipping, messages, catalog (all 7). **Proposal**: all 7.

## Next
"#;
        let parsed = parse_doc("docs/design/test.md", md);
        assert_eq!(parsed.questions.len(), 3);
        assert_eq!(
            parsed.questions[0].title,
            "What happens if the scratch DB is out of sync with `schema.sql` (e.g. after pulling new migrations)?"
        );
        assert_eq!(parsed.questions[1].title, "Who can hit `/api/sim`?");
        assert_eq!(
            parsed.questions[2].title,
            "Does the scratch stack run all 7 services, or only the ones the sim writes to?"
        );
    }

    #[test]
    fn no_open_questions_section_returns_empty() {
        let md = "# Foo\n\n**Status**: approved\n\n## Goal\n\nDone.\n";
        let parsed = parse_doc("docs/design/foo.md", md);
        assert!(parsed.questions.is_empty());
    }

    #[test]
    fn reopened_status_parses_over_shipped() {
        // Status line "reopened — original scope shipped ..." must
        // classify as Reopened (not Shipped). The reopened marker
        // wins over any trailing shipped-history prose.
        let md = "# Foo\n\n**Status**: reopened — original scope shipped 2026-04-18; new proposal pending\n";
        let parsed = parse_doc("docs/design/foo.md", md);
        assert_eq!(parsed.status, DocStatus::Reopened);
        assert!(!parsed.status.forbids_open_questions());
    }

    #[test]
    fn unresolved_questions_detected_anywhere_in_doc() {
        // Unresolved `### Qn:` headings outside `## Open questions`
        // still surface — the validator looks doc-wide so an author
        // can't quietly park live questions next to a proposal.
        let md = r#"# Foo

**Status**: shipped — done.

## Proposal

Some proposal.

### Q8: Is this open? (open)

Body.

### Q9: And this one?

Body.

## Open questions

### Q10: Already resolved (resolved)

Body.
"#;
        let parsed = parse_doc("docs/design/foo.md", md);
        assert_eq!(parsed.unresolved_questions.len(), 2);
        assert!(parsed.unresolved_questions[0].starts_with("Q8:"));
        assert!(parsed.unresolved_questions[1].starts_with("Q9:"));
    }

    #[test]
    fn resolved_marker_excludes_from_unresolved() {
        let md = r#"# Foo

**Status**: shipped

## Open questions

### Q1: All done (resolved)

Body.
"#;
        let parsed = parse_doc("docs/design/foo.md", md);
        assert!(parsed.unresolved_questions.is_empty());
    }

    #[test]
    fn word_count_is_rough() {
        let md = "# Foo\n\nOne two three four five.";
        let parsed = parse_doc("docs/design/foo.md", md);
        // "# Foo One two three four five." → 7 whitespace-separated
        // tokens (# counts as one).
        assert!(parsed.word_count >= 5);
    }

    #[test]
    fn content_html_is_rendered() {
        let md = "# Foo\n\n**Bold** text.";
        let parsed = parse_doc("docs/design/foo.md", md);
        assert!(parsed.content_html.contains("<h1>"));
        assert!(parsed.content_html.contains("<strong>Bold</strong>"));
    }

    #[test]
    fn slug_strips_non_alnum() {
        assert_eq!(
            slug_for("docs/design/correctness-protocol.md"),
            "correctness-protocol"
        );
        assert_eq!(slug_for("docs/design/foo_bar.md"), "foo-bar");
    }

    #[test]
    fn parse_h3_anchor_extracts_q_number() {
        assert_eq!(
            parse_h3_anchor("Q1: Where does boss-sim-api live?"),
            Some((
                "Q1".to_string(),
                "Where does boss-sim-api live?".to_string()
            ))
        );
        assert_eq!(
            parse_h3_anchor("Q42: Deep question"),
            Some(("Q42".to_string(), "Deep question".to_string()))
        );
        assert_eq!(parse_h3_anchor("Just a heading"), None);
        assert_eq!(parse_h3_anchor("Q: missing number"), None);
    }

    #[test]
    fn parses_typical_design_doc_with_h3_questions() {
        // Minimal reproduction of the canonical design-doc shape
        // (status frontmatter + H3 Qn anchors + inline proposals)
        // to verify the parser handles the style the existing
        // docs actually use.
        let md = r#"# Design: Simulator UI

**Status**: design, pre-implementation.

## Goal

Some prose about the goal.

## Open questions

Each has a proposal attached.

### Q1: Where does boss-sim-api live?

New crate or feature-gated?

**Proposal**: new crate.

### Q2: Scratch stack scope?

All 7 services or subset?

**Proposal**: all 7.
"#;
        let parsed = parse_doc("docs/design/example-fixture.md", md);
        assert_eq!(parsed.title, "Design: Simulator UI");
        assert_eq!(parsed.status, DocStatus::InReview);
        assert_eq!(parsed.questions.len(), 2);
        assert!(!parsed.used_fallback_anchors);
        assert_eq!(parsed.questions[0].anchor, "Q1");
        assert_eq!(parsed.questions[0].proposal.as_deref(), Some("new crate."));
    }
}
