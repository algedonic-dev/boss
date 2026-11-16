//! Packaging is a gate cost: preserve cases while linking at most 32 integration executables.
//! tree-wide pin — new test files must not silently restore one executable per file.

use boss_testing::repo_root;
use std::process::{Command, Output};

const READER: &str = "infra/gate-runner/test-targets.py";

fn read_membership(root: &std::path::Path) -> Output {
    Command::new("python3")
        .arg(repo_root().join(READER))
        .arg("--json")
        .current_dir(root)
        .output()
        .unwrap()
}

struct ProcessCalls(bool);
impl<'ast> syn::visit::Visit<'ast> for ProcessCalls {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*call.func
            && path.path.segments.last().is_some_and(|s| {
                // Construct this inspected name without spelling a subscriber
                // call in this detector's own source: the separate capture
                // ownership pin intentionally scans source text.
                s.ident == ["set", "global", "default"].join("_")
                    || matches!(
                        s.ident.to_string().as_str(),
                        "current_exe" | "set_current_dir" | "set_var" | "remove_var"
                    )
            })
        {
            self.0 = true;
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if matches!(call.method.to_string().as_str(), "init" | "try_init") {
            self.0 = true;
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

fn owns_process_state(text: &str) -> bool {
    let ast = syn::parse_file(text).unwrap();
    let mut calls = ProcessCalls(false);
    syn::visit::Visit::visit_file(&mut calls, &ast);
    calls.0
}

#[test]
fn method_subscriber_initializers_own_their_process() {
    for method in ["init", "try_init"] {
        let fixture = format!("fn sample() {{ tracing_subscriber::fmt().{method}(); }}");
        assert!(
            owns_process_state(&fixture),
            "{method} installs process state"
        );
    }
}

#[test]
fn every_original_file_has_exactly_one_compiled_owner() {
    let out = read_membership(&repo_root());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let membership: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!membership["owners"].as_object().unwrap().is_empty());
    let manifest: toml::Value = toml::from_str(
        &std::fs::read_to_string(repo_root().join("crates/core/boss-testing/Cargo.toml")).unwrap(),
    )
    .unwrap();
    for target in manifest["test"].as_array().unwrap() {
        if target.get("required-features").is_some() {
            let name = target["name"].as_str().unwrap();
            assert_eq!(
                membership["owners"][name], name,
                "feature gates stay on their original target"
            );
            assert_eq!(
                target["required-features"].as_array().unwrap(),
                &[toml::Value::String("postgres".into())]
            );
        }
    }
    // These callers address an exact unprefixed test name in their own
    // current executable; a module prefix would silently select no test.
    for name in [
        "gate_sh",
        "the_build_floor_lives_once",
        "integration_packaging",
    ] {
        assert_eq!(membership["owners"][name], name);
    }
    for (name, owner) in membership["owners"].as_object().unwrap() {
        let file = repo_root()
            .join("crates/core/boss-testing/tests")
            .join(format!("{name}.rs"));
        if owns_process_state(&std::fs::read_to_string(file).unwrap()) {
            assert_eq!(
                owner, name,
                "{name} owns process state and must remain dedicated"
            );
        }
    }
}

#[test]
fn unread_missing_duplicate_and_mixed_membership_refuse() {
    for (tag, group, extra) in [
        ("missing", "#[path = \"../a.rs\"]\nmod a;\n", true),
        (
            "duplicate",
            "#[path = \"../a.rs\"]\nmod a;\n#[path = \"../a.rs\"]\nmod a;\n",
            false,
        ),
        ("unread", "include!(\"../a.rs\");\n", false),
        ("harness", "#[path = \"../a.rs\"]\nmod a;\n", false),
        (
            "mixed",
            "#[path = \"../a.rs\"]\nmod a;\n#[path = \"../b.rs\"]\nmod b;\n",
            true,
        ),
    ] {
        let root = boss_testing::scratch_dir(&format!("integration-membership-{tag}"));
        let crate_dir = root.join("crates/core/boss-testing");
        boss_testing::create_dir(&crate_dir.join("tests/groups"));
        boss_testing::write_file(
            &crate_dir.join("Cargo.toml"),
            "[package]\nname=\"boss-testing\"\nautotests=false\n[[test]]\nname=\"group\"\npath=\"tests/groups/group.rs\"\n",
        );
        boss_testing::write_file(
            &crate_dir.join("tests/a.rs"),
            "//! tree-wide pin\n#[test]\nfn a() {}\n",
        );
        if tag == "harness" {
            let manifest = crate_dir.join("Cargo.toml");
            let text = std::fs::read_to_string(&manifest).unwrap() + "harness=false\n";
            boss_testing::write_file(&manifest, &text);
        }
        if extra {
            boss_testing::write_file(&crate_dir.join("tests/b.rs"), "#[test]\nfn b() {}\n");
        }
        boss_testing::write_file(&crate_dir.join("tests/groups/group.rs"), group);
        let out = read_membership(&root);
        assert!(
            !out.status.success(),
            "{tag} must refuse, got {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn integration_executable_count_stays_bounded() {
    let root = repo_root().join("crates/core/boss-testing");
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    let explicit = manifest["test"].as_array().unwrap().len();
    let automatic = manifest["package"]
        .get("autotests")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    let count = if automatic {
        std::fs::read_dir(root.join("tests"))
            .unwrap()
            .map(Result::unwrap)
            .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
            .count()
    } else {
        explicit
    };
    assert!(
        count <= 32,
        "{count} integration executables exceed the 32-target packaging budget"
    );
}

#[test]
fn group_members_are_sorted_and_missing_sources_name_the_placement_door() {
    let root = boss_testing::scratch_dir("integration-sorted-guidance");
    let dir = root.join("crates/core/boss-testing");
    boss_testing::create_dir(&dir.join("tests/groups"));
    boss_testing::write_file(
        &dir.join("Cargo.toml"),
        "[package]\nname=\"boss-testing\"\nautotests=false\n[[test]]\nname=\"group\"\npath=\"tests/groups/area.rs\"\n",
    );
    for name in ["a", "b"] {
        boss_testing::write_file(
            &dir.join(format!("tests/{name}.rs")),
            "#[test]\nfn sample() {}\n",
        );
    }
    let group = dir.join("tests/groups/area.rs");
    boss_testing::write_file(
        &group,
        "#[path = \"../b.rs\"]\nmod b;\n#[path = \"../a.rs\"]\nmod a;\n",
    );
    let out = read_membership(&root);
    assert!(!out.status.success(), "unsorted group must refuse");
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("sorted") && error.contains("area.rs"),
        "{error}"
    );
    boss_testing::write_file(
        &group,
        "#[path = \"../a.rs\"]\nmod a;\n#[path = \"../b.rs\"]\nmod b;\n",
    );
    assert!(read_membership(&root).status.success());
    boss_testing::write_file(&dir.join("tests/c.rs"), "#[test]\nfn sample() {}\n");
    let out = read_membership(&root);
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    for guidance in [
        "missing=['c']",
        "tests/groups/",
        "alphabetical",
        "process",
        "[[test]]",
    ] {
        assert!(error.contains(guidance), "missing {guidance:?}: {error}");
    }
    for name in ["no-session", "no_service"] {
        boss_testing::write_file(
            &dir.join(format!("tests/{name}.rs")),
            "#[test]\nfn sample() {}\n",
        );
    }
    boss_testing::write_file(
        &group,
        "#[path = \"../a.rs\"]\nmod a;\n#[path = \"../b.rs\"]\nmod b;\n#[path = \"../c.rs\"]\nmod c;\n#[path = \"../no-session.rs\"]\nmod no_session;\n#[path = \"../no_service.rs\"]\nmod no_service;\n",
    );
    let formatted = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "files"])
        .arg(&group)
        .output()
        .unwrap();
    assert!(
        formatted.status.success(),
        "{}",
        String::from_utf8_lossy(&formatted.stderr)
    );
    let actual = std::fs::read_to_string(&group).unwrap();
    assert!(actual.find("mod no_service;").unwrap() < actual.find("mod no_session;").unwrap());
    let out = read_membership(&root);
    assert!(
        out.status.success(),
        "actual rustfmt module order must be accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
