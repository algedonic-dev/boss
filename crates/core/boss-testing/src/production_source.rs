//! The production part of a Rust source file, for a tree-wide pin that
//! must not count what a test does (design 6805c764 car 2; backlog
//! 2710c8fc).
//!
//! A pin that reads every `.rs` under `crates/*/src` for how a request
//! is SENT meets two kinds of code that look alike on the page: the
//! request a service makes, and the request a `#[cfg(test)]` module
//! makes to a router in the same file to prove it. Cutting a file at its
//! first `#[cfg(test)]` is wrong both ways — a test module in the middle
//! of a file (boss-inventory's `items.rs` has one between two production
//! functions) hides the production code after it, and a
//! `#[cfg(test)] mod x;` declaration at the top hides the whole file. So
//! this parses the file and blanks exactly the items carrying a test
//! `cfg`, in any of the shapes `leaked_policy` recognises, line for line,
//! so every line number a pin reports is still the file's own.

use syn::spanned::Spanned;

/// `source` with every line of every `#[cfg(test)]` item (at any depth)
/// replaced by an empty line. Line numbers are preserved.
pub fn production_text(source: &str) -> Result<String, syn::Error> {
    let parsed = syn::parse_file(source)?;
    let mut finder = TestItems { ranges: Vec::new() };
    syn::visit::Visit::visit_file(&mut finder, &parsed);
    Ok(source
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let n = i + 1;
            if finder.ranges.iter().any(|(a, b)| (*a..=*b).contains(&n)) {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `source` with every comment — `//` to end of line (doc comments
/// included) and `/* … */`, nested — replaced by spaces, string and char
/// literals left whole, so a scan asking "does this CODE name X" is not
/// answered by a comment that names it (review of 39949355, M7: a
/// comment saying `MachineClient` satisfied the sender pin). Line breaks
/// are kept, so line numbers stay the file's own.
pub fn without_comments(source: &str) -> String {
    let c: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let blank = |ch: char| if ch == '\n' { '\n' } else { ' ' };
    while i < c.len() {
        match c[i] {
            '/' if c.get(i + 1) == Some(&'/') => {
                while i < c.len() && c[i] != '\n' {
                    out.push(' ');
                    i += 1;
                }
            }
            '/' if c.get(i + 1) == Some(&'*') => {
                let mut depth = 0usize;
                while i < c.len() {
                    if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                        depth += 1;
                        out.push_str("  ");
                        i += 2;
                    } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        out.push_str("  ");
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        out.push(blank(c[i]));
                        i += 1;
                    }
                }
            }
            'r' if matches!(c.get(i + 1), Some('"') | Some('#'))
                && (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_')) =>
            {
                // A raw string: r"…", r#"…"#, with as many hashes.
                let mut j = i + 1;
                let mut hashes = 0;
                while c.get(j) == Some(&'#') {
                    hashes += 1;
                    j += 1;
                }
                if c.get(j) != Some(&'"') {
                    out.push(c[i]);
                    i += 1;
                    continue;
                }
                j += 1;
                while j < c.len() {
                    if c[j] == '"' && (0..hashes).all(|k| c.get(j + 1 + k) == Some(&'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                out.extend(&c[i..j.min(c.len())]);
                i = j;
            }
            '"' => {
                let mut j = i + 1;
                while j < c.len() && c[j] != '"' {
                    j += if c[j] == '\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(c.len());
                out.extend(&c[i..end]);
                i = end;
            }
            '\'' if c.get(i + 2) == Some(&'\'') || (c.get(i + 1) == Some(&'\\')) => {
                // A char literal ('"', '\'', '\n'); a lifetime has no
                // closing quote and falls through as ordinary text.
                let mut j = i + 1;
                if c.get(j) == Some(&'\\') {
                    j += 2;
                    while j < c.len() && c[j] != '\'' {
                        j += 1;
                    }
                } else {
                    j += 1;
                }
                let end = (j + 1).min(c.len());
                out.extend(&c[i..end]);
                i = end;
            }
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

struct TestItems {
    /// 1-based inclusive line ranges of the test items found.
    ranges: Vec<(usize, usize)>,
}

impl<'ast> syn::visit::Visit<'ast> for TestItems {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs: &[syn::Attribute] = match item {
            syn::Item::Mod(i) => &i.attrs,
            syn::Item::Fn(i) => &i.attrs,
            syn::Item::Impl(i) => &i.attrs,
            syn::Item::Struct(i) => &i.attrs,
            syn::Item::Enum(i) => &i.attrs,
            syn::Item::Trait(i) => &i.attrs,
            syn::Item::Const(i) => &i.attrs,
            syn::Item::Static(i) => &i.attrs,
            syn::Item::Use(i) => &i.attrs,
            _ => &[],
        };
        if crate::leaked_policy::has_cfg_test(attrs) {
            let span = item.span();
            self.ranges.push((span.start().line, span.end().line));
            return;
        }
        syn::visit::visit_item(self, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_module_in_the_middle_is_blanked_and_what_follows_is_kept() {
        let src = "fn a() { send(\"x-boss-user\"); }\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   \x20   fn t() { let s = \"{ unbalanced\"; send(\"x-boss-user\"); }\n\
                   }\n\
                   fn b() { send(\"x-boss-user\"); }\n";
        let out = production_text(src).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("x-boss-user"), "{out}");
        assert_eq!(lines[1..5], ["", "", "", ""], "{out}");
        assert!(lines[5].contains("fn b()"), "{out}");
    }

    #[test]
    fn comments_go_and_literals_stay() {
        let src = "let a = MachineClient; // MachineClient here\n\
                   /// MachineClient in a doc\n\
                   let u = \"http://x // not a comment\"; /* a /* nested */ MachineClient */ let q = '\"';\n\
                   let r = r#\"// MachineClient \"quoted\"\"#; fn f<'a>(x: &'a str) {}\n";
        let out = without_comments(src);
        assert_eq!(out.lines().count(), src.lines().count());
        assert_eq!(out.matches("MachineClient").count(), 2, "{out}");
        assert!(out.contains("\"http://x // not a comment\""), "{out}");
        assert!(out.contains("let q = '\"';"), "{out}");
        assert!(out.contains("fn f<'a>(x: &'a str) {}"), "{out}");
        assert!(!out.contains("nested"), "{out}");
    }

    #[test]
    fn a_test_only_function_and_a_cfg_any_test_are_blanked() {
        let src = "#[cfg(any(test, feature = \"x\"))]\n\
                   fn t() {}\n\
                   #[cfg(feature = \"y\")]\n\
                   fn kept() {}\n";
        let out = production_text(src).unwrap();
        assert!(!out.contains("fn t()"), "{out}");
        assert!(out.contains("fn kept()"), "{out}");
    }
}
