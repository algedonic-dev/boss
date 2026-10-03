//! tree-wide pin — it reads shared domain definitions and scans every crates tests directory,
//! so fixture changes outside boss-testing must run this check in every scoped gate.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use syn::visit::Visit;

struct Exhaustive {
    sites: Vec<(String, usize)>,
    shapes: BTreeMap<String, BTreeSet<String>>,
}

fn canonical(name: &str) -> Option<String> {
    matches!(name, "Job" | "Step" | "StepField").then(|| name.to_owned())
}

impl Exhaustive {
    fn domain_shapes() -> BTreeMap<String, BTreeSet<String>> {
        let root = boss_testing::repo_root();
        let source =
            std::fs::read_to_string(root.join("crates/core/boss-core/src/job.rs")).unwrap();
        let ast = syn::parse_file(&source).unwrap();
        let mut shapes = BTreeMap::new();
        for item in ast.items {
            if let syn::Item::Struct(item) = item
                && let Some(kind) = canonical(&item.ident.to_string())
            {
                let fields = item
                    .fields
                    .iter()
                    .map(|field| field.ident.as_ref().unwrap().to_string())
                    .collect();
                shapes.insert(kind, fields);
            }
        }
        assert_eq!(shapes.len(), 3, "every owning struct must be read");
        assert!(
            shapes
                .values()
                .all(|fields: &BTreeSet<String>| !fields.is_empty())
        );
        shapes
    }

    fn new() -> Self {
        Self {
            sites: Vec::new(),
            shapes: Self::domain_shapes(),
        }
    }

    fn macro_expressions(&mut self, stream: proc_macro2::TokenStream) {
        use proc_macro2::{Delimiter, TokenTree};
        let mut previous = None;
        for token in stream {
            if let TokenTree::Group(group) = &token {
                if group.delimiter() == Delimiter::Brace
                    && let Some(TokenTree::Ident(name)) = &previous
                {
                    let expression: proc_macro2::TokenStream =
                        [TokenTree::Ident(name.clone()), token.clone()]
                            .into_iter()
                            .collect();
                    if let Ok(expression) = syn::parse2::<syn::ExprStruct>(expression) {
                        self.visit_expr_struct(&expression);
                        previous = Some(token);
                        continue;
                    }
                }
                self.macro_expressions(group.stream());
            }
            previous = Some(token);
        }
    }
}

#[test]
fn an_alias_outside_a_nested_helper_is_still_the_domain_fixture() {
    let fields = Exhaustive::domain_shapes()["Job"]
        .iter()
        .map(|field| format!("{field}: value"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        "use boss_core::job::Job as Packet; mod helper {{ struct Packet {{ dir: PathBuf }} }} fn outer() {{ let packet = Packet {{ {fields} }}; }}"
    );
    let ast = syn::parse_file(&source).unwrap();
    let mut visitor = Exhaustive::new();
    visitor.visit_file(&ast);
    assert_eq!(visitor.sites, vec![("Job".into(), 1)]);
}

#[test]
fn an_inner_import_overrides_an_outer_helper_type() {
    let fields = Exhaustive::domain_shapes()["Job"]
        .iter()
        .map(|field| format!("{field}: value"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        "struct Job {{ dir: PathBuf }} mod actual {{ use boss_core::job::Job; fn f() {{ Job {{ {fields} }} }} }}"
    );
    let ast = syn::parse_file(&source).unwrap();
    let mut visitor = Exhaustive::new();
    visitor.visit_file(&ast);
    assert_eq!(visitor.sites, vec![("Job".into(), 1)]);
}

#[test]
fn ast_shapes_cover_every_alias_and_macro_without_repeating_domain_fields() {
    let shapes = Exhaustive::domain_shapes();
    for (kind, shape) in &shapes {
        let fields = shape
            .iter()
            .map(|field| format!("{field}: value"))
            .collect::<Vec<_>>()
            .join(",");
        for expression in [
            format!("Alias {{ {fields} }}"),
            format!("vec![Alias {{ {fields} }}]"),
            format!("other::Alias {{ {fields} }}"),
        ] {
            let source = format!(
                "use boss_core::job::{kind} as Alias; mod helper {{ struct Alias {{ dir: PathBuf }} }} fn f() {{ let item = {expression}; }}"
            );
            let mut visitor = Exhaustive::new();
            visitor.visit_file(&syn::parse_file(&source).unwrap());
            assert_eq!(visitor.sites, vec![(kind.clone(), 1)], "{source}");
        }
        let source = format!("fn f() {{ let item = Alias {{ {fields}, ..base }}; }}");
        let mut visitor = Exhaustive::new();
        visitor.visit_file(&syn::parse_file(&source).unwrap());
        assert!(visitor.sites.is_empty(), "constructor bases remain legal");
    }
    let mut visitor = Exhaustive::new();
    visitor.visit_file(
        &syn::parse_file("struct Job { dir: PathBuf } fn f() { Job { dir: value } }").unwrap(),
    );
    assert!(visitor.sites.is_empty(), "unrelated helper remains legal");
}

impl<'ast> Visit<'ast> for Exhaustive {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.macro_expressions(node.tokens.clone());
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        use syn::spanned::Spanned;
        let fields: BTreeSet<String> = node
            .fields
            .iter()
            .filter_map(|field| match &field.member {
                syn::Member::Named(name) => Some(name.to_string()),
                _ => None,
            })
            .collect();
        if node.rest.is_none() {
            for (kind, shape) in &self.shapes {
                // This is deliberately independent of name resolution. A
                // valid exhaustive domain expression has every owning field,
                // whatever alias or macro body spells it. An unrelated struct
                // with the COMPLETE same shape is conservatively refused.
                if shape.is_subset(&fields) {
                    self.sites.push((kind.clone(), node.span().start().line));
                }
            }
        }
        syn::visit::visit_expr_struct(self, node);
    }
}

fn rust_files(directory: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("directory readable") {
        let path = entry.expect("entry readable").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                rust_files(&path, files);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path.components().any(|part| part.as_os_str() == "tests")
        {
            files.push(path);
        }
    }
}

#[test]
fn ast_independently_checks_that_no_domain_fixture_was_missed() {
    let root = boss_testing::repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    assert!(!files.is_empty());
    let shapes = Exhaustive::domain_shapes();
    let mut findings = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path).expect("source readable");
        let ast = syn::parse_file(&source).expect("valid Rust");
        let mut visitor = Exhaustive {
            sites: Vec::new(),
            shapes: shapes.clone(),
        };
        visitor.visit_file(&ast);
        for (kind, line) in visitor.sites {
            findings.push(format!("{}:{line}: {kind}", path.display()));
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

#[test]
fn exhaustive_fixture_scanner_distinguishes_expressions_from_rust_syntax() {
    let root = boss_testing::repo_root();
    let output = Command::new("python3")
        .arg(root.join("infra/lint/lib/extensible-fixtures.py"))
        .arg("--self-test")
        .output()
        .expect("scanner starts");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn every_integration_fixture_uses_a_constructor_base() {
    let root = boss_testing::repo_root();
    let output = Command::new("bash")
        .arg(root.join("infra/lint/test-fixtures-use-constructor-bases.sh"))
        .output()
        .expect("lint starts");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_lint_covers_nested_test_directories_and_leaves_production_literals_legal() {
    let root = boss_testing::repo_root();
    let tree = boss_testing::scratch_dir("extensible-fixture-scope");
    let tests = tree.join("crates/any/tier/tests/nested");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::create_dir_all(tree.join("crates/any/tier/src")).unwrap();
    let scanner = root.join("infra/lint/lib/extensible-fixtures.py");
    let run = || {
        Command::new("python3")
            .arg(&scanner)
            .arg(&tree)
            .output()
            .unwrap()
    };
    let fixture = tests.join("fixture.rs");
    std::fs::write(
        tree.join("crates/any/tier/src/mapper.rs"),
        "fn f() { let j = Job { id: x }; }\n",
    )
    .unwrap();
    std::fs::write(&fixture, "fn f() { let j = Job { id: x, ..base }; }\n").unwrap();
    assert!(run().status.success());
    std::fs::write(
        &fixture,
        "fn f() {\n let j = Job { id: x, nested: Other { ..base } };\n}\n",
    )
    .unwrap();
    let refused = run();
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("tests/nested/fixture.rs:2: exhaustive Job")
    );
    std::fs::remove_file(&fixture).unwrap();
    assert!(
        !run().status.success(),
        "zero integration files cannot be a pass"
    );
}
