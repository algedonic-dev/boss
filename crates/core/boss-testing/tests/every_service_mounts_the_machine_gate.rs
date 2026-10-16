//! Every HTTP server in the tree mounts the machine gate, and every
//! probe a reader outside the token can send is exempt on the service
//! it targets (design 6805c764, car 1; backlog 2710c8fc).
//!
//! WHAT WAS MEASURED (triage of 2710c8fc, origin/main 165619f7,
//! 2026-09-25). 27 binaries serve HTTP and the machine gate was mounted
//! in ONE, the jobs API, where it was dormant besides. Anyone on the LAN
//! or the WireGuard mesh could assert `x-boss-user` at any of the other
//! 26 ports. David's decision is that every service port requires the
//! machine token; the design lands it in five cars, and this pin is
//! the first car's floor: a service that ships without the gate cannot
//! be moved to `report` or `enforce` by the mode file, so a 28th server
//! that forgets it is the hole this item was filed about, reopened in
//! silence.
//!
//! THE SHAPES THIS HOLDS.
//!
//!   1. Known serving calls and unaccounted TCP binds outside test items
//!      are inventoried through Rust syntax. A served app must have its
//!      own lexical mount relationship — a neighbouring mount is no
//!      proof — or the listener is named in [`UNGATED`] with the reason.
//!      Opaque macro serving syntax refuses rather than inventing that
//!      relationship. This is a source pin, not complete runtime listener
//!      ownership evidence; design2afd8e21 owns that separate observation.
//!      A file
//!      its parent declares only as `#[cfg(test)] mod …;` is all test
//!      region (`test_only_modules`).
//!   2. Every mount names its service by its `boss-ports` name, so the
//!      probe check below can find the mount a port leads to.
//!   3. Every `httpGet` probe in `infra/cluster/manifests/` whose port is
//!      a `boss-ports` service, and every path the off-cluster watchdog
//!      (`infra/forge/cluster-watchdog.sh`) reads from the jobs API, is
//!      in that service's exact-path exemption list. A probe answered
//!      401 restarts pods; a watchdog answered 401 goes blind, and an arm
//!      that needs the patient is not an arm (CLAUDE.md §Diagnosis). This
//!      is a fact that lives twice — the probe in the manifest, the
//!      exemption in the binary — so it is pinned (CLAUDE.md §9a).
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Servers that deliberately do NOT mount the gate, each with the
/// reason — ONE definition in `boss_core::machine_gate`, which the
/// credential broker's rotation roster reads too, so a port this pin
/// excuses is never counted as a gate that must accept a token (review
/// 70d449f9 of car 3, B1). A stale row (the file no longer serves) fails
/// the pin too.
use boss_core::machine_gate::UNGATED;

const ROLE_SERVER_ADAPTER: &str = "crates/core/boss-policy-client/src/role_service.rs";
mod server_gate_scan;

#[test]
fn serving_an_app_requires_its_own_unshadowed_mount() {
    let good = "fn main() { let app = machine_gate::mount(app); axum::serve(listener, app); }";
    assert!(server_gate_scan::scan(good).unwrap()[0].gated);
    for source in [
        "fn main() { let mut app = machine_gate::mount(app); { let mut app = other; replace(&mut app); } axum::serve(listener, app); }",
        "fn main() { let mut app = machine_gate::mount(app); replace(&mut app); let app = machine_gate::mount(app); axum::serve(listener, app); }",
    ] {
        assert!(server_gate_scan::scan(source).unwrap()[0].gated);
    }

    for source in [
        "fn main() { let app = Router::new(); { let app = machine_gate::mount(app); } axum::serve(listener, app); }",
        "fn main() { let app = machine_gate::mount(app); let app = Router::new(); axum::serve(listener, app); }",
        "fn main() { let app = machine_gate::mount(app); axum::serve(listener, other); }",
        "fn main() { let mut app = machine_gate::mount(app); app = Router::new(); axum::serve(listener, app); }",
        "fn main() { let app = machine_gate::mount(app); axum_server::bind(addr).serve(other); }",
        "fn main() { let app = machine_gate::mount(app); let f = |app| { axum::serve(listener, app); }; }",
        "fn main() { let app = machine_gate::mount(app); for app in others { axum::serve(listener, app); } }",
        "fn main() { let app = machine_gate::mount(app); if let Some(app) = other { axum::serve(listener, app); } }",
        "fn main() { let mut app = machine_gate::mount(app); replace(&mut app); axum::serve(listener, app); }",
        "fn main() { let mut app = machine_gate::mount(app); { app = other; } axum::serve(listener, app); }",
        "fn main() { let mut app = machine_gate::mount(app); if condition { replace(&mut app); } axum::serve(listener, app); }",
        "use axum::serve as http; fn endpoint() { let app = machine_gate::mount(app); let http = other; http(listener, app); }",
    ] {
        assert!(
            !server_gate_scan::scan(source).unwrap()[0].gated,
            "unguarded served app: {source}"
        );
    }
}

#[test]
fn the_private_tenant_callback_is_explicitly_classified() {
    let rows = private_listeners(&repo_root());
    let row = rows
        .iter()
        .find(|u| u.file == "crates/tenants/boss-brewery-engine/src/lib.rs")
        .expect("the inventoried callback needs an explicit bounded classification");
    assert_eq!(row.function, "private_callback_listener");
    let nested = server_gate_scan::scan(
        "mod foreign { fn private_callback_listener() { std::net::TcpListener::bind(addr); } }",
    )
    .unwrap();
    assert_ne!(
        row.function, nested[0].function,
        "a same-named function in another module is not the callback"
    );
}

#[test]
fn pattern_bindings_cannot_inherit_an_outer_mount() {
    let mut wrong = Vec::new();
    for (source, expected) in [
        (
            "fn main() { let app = machine_gate::mount(app); let (app,) = (ungated,); axum::serve(listener, app); }",
            vec![false],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); let State { app } = ungated; axum::serve(listener, app); }",
            vec![false],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); let [app] = ungated; axum::serve(listener, app); }",
            vec![false],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); match ungated { app => { axum::serve(listener, app); } } }",
            vec![false],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); axum::serve(listener, app); }",
            vec![true],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); let (app,) = (ungated,); let app = machine_gate::mount(app); axum::serve(listener, app); }",
            vec![true],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); { let (app,) = (ungated,); axum::serve(listener, app); } axum::serve(listener, app); }",
            vec![false, true],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); match ungated { Some(app) => axum::serve(listener, app), _ => axum::serve(listener, app) } axum::serve(listener, app); }",
            vec![false, true, true],
        ),
        (
            "fn main() { let mut app = machine_gate::mount(app); match other { _ => { app = ungated; } } axum::serve(listener, app); }",
            vec![false],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); while let Some(app) = other { axum::serve(listener, app); } axum::serve(listener, app); }",
            vec![false, true],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); let app = { axum::serve(listener, app); ungated }; axum::serve(listener, app); }",
            vec![true, false],
        ),
        (
            "fn main() { let app = machine_gate::mount(app); let Some(app) = other else { axum::serve(listener, app); return; }; axum::serve(listener, app); }",
            vec![true, false],
        ),
    ] {
        let actual = server_gate_scan::scan(source)
            .unwrap()
            .into_iter()
            .map(|row| row.gated)
            .collect::<Vec<_>>();
        if actual != expected {
            wrong.push((source, actual, expected));
        }
    }
    assert!(wrong.is_empty(), "false pattern provenance: {wrong:#?}");
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateListener {
    file: String,
    function: String,
    bound: String,
    why: String,
}

fn private_listeners(root: &Path) -> Vec<PrivateListener> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Inventory {
        version: u32,
        engine_crate: String,
        private_listener: Vec<PrivateListener>,
    }
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(root.join("crates/tenants")).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_dir() {
            continue;
        }
        let file = entry.path().join("listener-inventory.toml");
        let metadata = match std::fs::symlink_metadata(&file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("cannot read listener inventory {}: {error}", file.display()),
        };
        assert!(
            metadata.file_type().is_file(),
            "inventory must be a regular file"
        );
        let inventory: Inventory =
            toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(inventory.version, 1);
        assert!(
            inventory.engine_crate.starts_with("boss-")
                && inventory
                    .engine_crate
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
        assert_eq!(
            entry.file_name().to_str().unwrap(),
            inventory.engine_crate,
            "inventory belongs to its own tenant crate"
        );
        let prefix = format!("crates/tenants/{}/src/", inventory.engine_crate);
        for row in inventory.private_listener {
            assert!(
                row.file.starts_with(&prefix)
                    && !Path::new(&row.file)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
            );
            assert!(
                !row.function.is_empty()
                    && row
                        .function
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            );
            assert_eq!(row.bound, "loopback");
            assert!(!row.why.trim().is_empty());
            assert!(
                seen.insert((row.file.clone(), row.function.clone())),
                "duplicate listener declaration"
            );
            rows.push(row);
        }
    }
    rows
}

#[test]
fn private_inventory_cannot_excuse_other_owners_or_bounds() {
    for (owner, file, bound) in [
        (
            "boss-other",
            "crates/tenants/boss-other/src/lib.rs",
            "loopback",
        ),
        (
            "boss-engine",
            "crates/core/boss-core/src/lib.rs",
            "loopback",
        ),
        (
            "boss-engine",
            "crates/tenants/boss-other/src/lib.rs",
            "loopback",
        ),
        (
            "boss-engine",
            "crates/tenants/boss-engine/src/../lib.rs",
            "loopback",
        ),
        (
            "boss-engine",
            "crates/tenants/boss-engine/src/lib.rs",
            "LAN",
        ),
    ] {
        let root = boss_testing::scratch_dir("private-listener-owner");
        std::fs::create_dir_all(root.join("crates/tenants/boss-engine")).unwrap();
        let document = format!(
            "version = 1\nengine_crate = {owner:?}\n[[private_listener]]\nfile = {file:?}\nfunction = \"callback\"\nbound = {bound:?}\nwhy = \"fixture\"\n"
        );
        std::fs::write(
            root.join("crates/tenants/boss-engine/listener-inventory.toml"),
            document,
        )
        .unwrap();
        assert!(std::panic::catch_unwind(|| private_listeners(&root)).is_err());
    }
}

#[test]
fn alternate_listener_forms_cannot_disappear_from_the_inventory() {
    for source in [
        "use axum::serve; fn main() { serve(listener, app); }",
        "fn endpoint() { http(listener, app); } use axum::serve as http;",
        "fn endpoint() { http(listener, app); use axum::serve as http; }",
        "use axum::{serve as http}; fn main() { http(listener, app); }",
        "fn main() { axum_server::bind(addr).serve(app); }",
        "fn main() { hyper::Server::bind(&addr).serve(app); }",
        "fn callback() { let listener = std::net::TcpListener::bind(addr); }",
        "use std::net::TcpListener as Listener; fn callback() { Listener::bind(addr); }",
        "fn main() { fn hidden() { std::net::TcpListener::bind(addr); } }",
    ] {
        assert_eq!(server_calls("service.rs", source), 1, "missing {source}");
    }
}

#[test]
fn literal_macro_text_is_not_a_listener_and_a_foreign_mount_proves_nothing() {
    assert_eq!(
        server_calls(
            "service.rs",
            "fn main() { tracing::info!(\"axum::serve(\"); }"
        ),
        0
    );
    let foreign =
        "fn main() { let app = other::machine_gate::mount(app); axum::serve(listener, app); }";
    assert!(!server_gate_scan::scan(foreign).unwrap()[0].gated);
}

#[test]
fn mount_calls_require_unshadowed_lexical_imports() {
    let prefix = "use boss_core::machine_gate::mount as gate;";
    for (body, expected) in [
        (
            "fn main() { let app = gate(app); axum::serve(listener, app); }",
            vec![true],
        ),
        (
            "fn main() { let gate = |app| app; let app = gate(app); axum::serve(listener, app); }",
            vec![false],
        ),
        (
            "fn main() { let (gate,) = (identity,); let app = gate(app); axum::serve(listener, app); }",
            vec![false],
        ),
        (
            "fn main(gate: F) { let app = gate(app); axum::serve(listener, app); }",
            vec![false],
        ),
        (
            "fn main((gate,): (F,)) { let app = gate(app); axum::serve(listener, app); }",
            vec![false],
        ),
        (
            "impl Service { fn run(gate: F) { let app = gate(app); axum::serve(listener, app); } }",
            vec![false],
        ),
        (
            "fn main() { fn run(gate: F) { let app = gate(app); axum::serve(listener, app); } }",
            vec![false],
        ),
        (
            "fn main() { let run = |gate| { let app = gate(app); axum::serve(listener, app); }; let app = gate(app); axum::serve(listener, app); }",
            vec![false, true],
        ),
        (
            "fn main() { match identity { gate => { let app = gate(app); axum::serve(listener, app); } } let app = gate(app); axum::serve(listener, app); }",
            vec![false, true],
        ),
        (
            "fn main() { { let gate = identity; let app = gate(app); axum::serve(listener, app); } let app = gate(app); axum::serve(listener, app); }",
            vec![false, true],
        ),
        (
            "fn main() { let gate = { let app = gate(app); axum::serve(listener, app); identity }; let app = gate(app); axum::serve(listener, app); }",
            vec![true, false],
        ),
        (
            "fn main() { let app = gate(app); axum::serve(listener, app); fn gate(app: App) -> App { app } }",
            vec![false],
        ),
    ] {
        let source = format!("{prefix} {body}");
        let actual: Vec<_> = server_gate_scan::scan(&source)
            .unwrap()
            .iter()
            .map(|s| s.gated)
            .collect();
        assert_eq!(actual, expected, "mount identity differs: {source}");
    }
}

#[test]
fn nested_functions_inherit_items_without_capturing_local_values() {
    mod lexical {
        fn original(app: u8) -> u8 {
            app + 1
        }
        use original as gate;
        pub fn observed() -> (u8, u8) {
            fn gate(app: u8) -> u8 {
                app
            }
            fn run(app: u8) -> u8 {
                gate(app)
            }
            (self::gate(7), run(7))
        }
    }
    assert_eq!(lexical::observed(), (8, 7));
    for (body, expected) in [
        (
            "fn main() { fn gate(app: App) -> App { app } fn run(app: App, listener: Listener) { let app = gate(app); axum::serve(listener, app); } }",
            vec![false],
        ),
        (
            "fn main() { fn run(app: App, listener: Listener) { let app = gate(app); axum::serve(listener, app); } fn gate(app: App) -> App { app } }",
            vec![false],
        ),
        (
            "fn main() { fn gate(app: App) -> App { app } fn outer() { fn run(app: App, listener: Listener) { let app = gate(app); axum::serve(listener, app); } } }",
            vec![false],
        ),
        (
            "fn main() { fn run(app: App, listener: Listener) { let app = gate(app); axum::serve(listener, app); } }",
            vec![true],
        ),
        (
            "fn main() { { fn gate(app: App) -> App { app } fn run(app: App, listener: Listener) { let app = gate(app); axum::serve(listener, app); } } fn run(app: App, listener: Listener) { let app = gate(app); axum::serve(listener, app); } }",
            vec![false, true],
        ),
        (
            "fn main() { let app = gate(app); fn run(listener: Listener) { axum::serve(listener, app); } }",
            vec![false],
        ),
    ] {
        let source = format!("use boss_core::machine_gate::mount as gate; {body}");
        let actual: Vec<_> = server_gate_scan::scan(&source)
            .unwrap()
            .iter()
            .map(|s| s.gated)
            .collect();
        assert_eq!(actual, expected, "nested item identity differs: {source}");
    }
}

#[test]
fn locally_declared_mount_namespaces_are_not_the_imported_gate() {
    mod lexical {
        mod original {
            pub fn mount(app: u8) -> u8 {
                app + 1
            }
        }
        use original as machine_gate;
        pub fn observed() -> (u8, u8, u8) {
            mod machine_gate {
                pub fn mount(app: u8) -> u8 {
                    app
                }
            }
            mod boss_core {
                pub mod machine_gate {
                    pub fn mount(app: u8) -> u8 {
                        app
                    }
                }
            }
            (
                self::machine_gate::mount(7),
                machine_gate::mount(7),
                boss_core::machine_gate::mount(7),
            )
        }
    }
    assert_eq!(lexical::observed(), (8, 7, 7));
    mod imported_local {
        mod machine_gate {
            pub fn mount(app: u8) -> u8 {
                app
            }
        }
        use machine_gate::mount as gate;
        pub fn observed() -> u8 {
            gate(7)
        }
    }
    assert_eq!(imported_local::observed(), 7);
    for body in [
        "fn main(app: App, listener: Listener) { mod machine_gate { pub fn mount(app: App) -> App { app } } let app = machine_gate::mount(app); axum::serve(listener, app); }",
        "fn main(app: App, listener: Listener) { mod boss_core { pub mod machine_gate { pub fn mount(app: App) -> App { app } } } let app = boss_core::machine_gate::mount(app); axum::serve(listener, app); }",
        "mod machine_gate { pub fn mount(app: App) -> App { app } } fn main(app: App, listener: Listener) { let app = machine_gate::mount(app); axum::serve(listener, app); }",
        "mod boss_core { pub mod machine_gate { pub fn mount(app: App) -> App { app } } } fn main(app: App, listener: Listener) { let app = boss_core::machine_gate::mount(app); axum::serve(listener, app); }",
        "mod machine_gate { pub fn mount<T>(app: T) -> T { app } } use machine_gate::mount as gate; fn main(app: App, listener: Listener) { let app = gate(app); axum::serve(listener, app); }",
        "mod boss_core { pub mod machine_gate { pub fn mount<T>(app: T) -> T { app } } } use boss_core::machine_gate::mount as gate; fn main(app: App, listener: Listener) { let app = gate(app); axum::serve(listener, app); }",
        "mod boss_core { pub mod machine_gate { pub fn mount<T>(app: T) -> T { app } } } use boss_core as core; fn main(app: App, listener: Listener) { let app = core::machine_gate::mount(app); axum::serve(listener, app); }",
    ] {
        let servers = server_gate_scan::scan(body).unwrap();
        assert_eq!(servers.len(), 1);
        assert!(
            !servers[0].gated,
            "local namespace supplied gate provenance: {body}"
        );
    }
}

#[test]
fn a_local_modules_import_cannot_change_the_enclosing_mount_identity() {
    mod lexical {
        fn original(app: u8) -> u8 {
            app + 1
        }
        fn foreign(app: u8) -> u8 {
            app
        }
        use foreign as gate;
        pub fn observed() -> (u8, u8) {
            mod helper {
                use super::original as gate;
                pub fn run(app: u8) -> u8 {
                    gate(app)
                }
            }
            (helper::run(7), gate(7))
        }
    }
    assert_eq!(lexical::observed(), (8, 7));
    for (source, expected) in [
        (
            "use foreign::mount as gate; fn main(app: App, listener: Listener) { mod helper { use boss_core::machine_gate::mount as gate; } let app = gate(app); axum::serve(listener, app); }",
            false,
        ),
        (
            "use boss_core::machine_gate::mount as gate; fn main(app: App, listener: Listener) { mod helper { use foreign::mount as gate; } let app = gate(app); axum::serve(listener, app); }",
            true,
        ),
    ] {
        let servers = server_gate_scan::scan(source).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].gated, expected,
            "module import escaped: {source}"
        );
    }
}

#[test]
fn known_opaque_macro_servers_are_visible_but_unproved() {
    for source in [
        "fn main() { tokio::select! { _ = axum_server::bind(addr).serve(app) => {} } }",
        "fn main() { tokio::select! { _ = hyper::Server::bind(&addr).serve(app) => {} } }",
        "use axum::serve as http; fn main() { tokio::select! { _ = http(listener, app) => {} } }",
    ] {
        let servers = server_gate_scan::scan(source).unwrap();
        assert_eq!(servers.len(), 1, "opaque server vanished: {source}");
        assert!(!servers[0].gated, "macro expansion was not proven");
    }
}

#[test]
fn a_builder_must_be_the_known_transport_as_well_as_receive_the_mounted_app() {
    for builder in ["axum_server::bind(addr)", "hyper::Server::bind(&addr)"] {
        let source =
            format!("fn main() {{ let app = machine_gate::mount(app); {builder}.serve(app); }}");
        assert!(server_gate_scan::scan(&source).unwrap()[0].gated);
    }
    let unknown = "fn main() { let app = machine_gate::mount(app); unknown_transport.serve(app); }";
    assert!(
        !server_gate_scan::scan(unknown).unwrap()[0].gated,
        "unknown adapter semantics must stay unproved"
    );
}

/// The report transport serves its caller's finished app. Count the
/// caller as the boundary that must mount the gate, and verify that the
/// transport cannot wrap, replace or otherwise consume that app first.
fn server_calls(relative: &str, source: &str) -> usize {
    if relative == ROLE_SERVER_ADAPTER {
        assert!(
            transparent_role_transport(source),
            "role-report transport changed the caller's app"
        );
        return 0;
    }
    server_gate_scan::scan(source)
        .expect("server source parses")
        .len()
}

fn transparent_role_transport(source: &str) -> bool {
    use syn::visit::Visit;
    #[derive(Default)]
    struct AppUses {
        references: usize,
        direct_serves: usize,
        total_serves: usize,
        shadows_app: bool,
    }
    impl<'a> Visit<'a> for AppUses {
        fn visit_pat_ident(&mut self, pat: &'a syn::PatIdent) {
            if pat.ident == "app" {
                self.shadows_app = true;
            }
            syn::visit::visit_pat_ident(self, pat);
        }
        fn visit_expr_path(&mut self, path: &'a syn::ExprPath) {
            if path.path.is_ident("app") {
                self.references += 1;
            }
            syn::visit::visit_expr_path(self, path);
        }
        fn visit_expr_call(&mut self, call: &'a syn::ExprCall) {
            if let syn::Expr::Path(path) = &*call.func
                && path
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect::<Vec<_>>()
                    == ["axum", "serve"]
            {
                self.total_serves += 1;
                if matches!(call.args.iter().nth(1), Some(syn::Expr::Path(app)) if app.path.is_ident("app"))
                {
                    self.direct_serves += 1;
                }
            }
            syn::visit::visit_expr_call(self, call);
        }
    }
    let Ok(file) = syn::parse_file(source) else {
        return false;
    };
    let Some(function) = file.items.iter().find_map(|item| match item {
        syn::Item::Fn(f) if f.sig.ident == "serve_with_refresh" => Some(f),
        _ => None,
    }) else {
        return false;
    };
    let mut whole = AppUses::default();
    whole.visit_file(&file);
    let mut transport = AppUses::default();
    transport.visit_block(&function.block);
    whole.total_serves == 1
        && transport.direct_serves == 1
        && transport.references == 1
        && !transport.shadows_app
}

#[test]
fn the_server_scan_counts_delegated_boundaries_and_refuses_a_changed_transport() {
    let direct = "fn serve_with_refresh(listener: L, app: A) { axum::serve(listener, app); }";
    assert!(transparent_role_transport(direct));
    assert!(!transparent_role_transport(&direct.replace(
        "axum::serve(listener, app)",
        "axum::serve(listener, another_app)"
    )));
    assert!(!transparent_role_transport(&direct.replace(
        "axum::serve(listener, app)",
        "let app = Router::new(); axum::serve(listener, app)"
    )));
    assert!(!transparent_role_transport(&direct.replace(
        "axum::serve(listener, app)",
        "inspect(app.clone()); axum::serve(listener, app)"
    )));
    assert!(!transparent_role_transport(&direct.replace(
        "axum::serve(listener, app)",
        "let serve = |app| axum::serve(listener, app); serve(another_app)"
    )));
    let boundary = "fn main() { role_service::serve_with_refresh(listener, app); }";
    assert_eq!(server_calls("service.rs", boundary), 1);
    assert_eq!(
        boundary.matches("machine_gate::mount(").count(),
        0,
        "a delegate without a gate is still refused"
    );
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules" || n == "tests")
            {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Use the shared item-aware reader: test code disappears, production
/// after it survives, and line numbers remain the source's own.
fn production_part(text: &str) -> String {
    let mut production =
        boss_testing::production_source::production_text(text).expect("production source parses");
    if text.ends_with('\n') {
        production.push('\n');
    }
    production
}

/// Every module a parent declares only under `#[cfg(test)]`, as the
/// paths it can live at: `<name>.rs` and the `<name>/` directory its
/// own children live in (or the `#[path]` it names). A file there is a
/// test's fixture in the same sense as the region `production_part`
/// cuts off — the compiler never builds it into the binary. Found the
/// day the gateway's read-only-session tests landed as `#[cfg(test)]
/// mod` files that serve a recording upstream (car a159e1ee's dock
/// re-gate, 2026-09-25).
fn test_only_modules(files: &[PathBuf]) -> Vec<PathBuf> {
    let decl = Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z0-9_]+)\s*;").unwrap();
    let path_attr = Regex::new(r#"^#\[path\s*=\s*"([^"]+)"\]$"#).unwrap();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let parent = f.parent().unwrap_or(Path::new(""));
        // A mod.rs-style file declares children beside it; any other
        // file declares them in the directory named after it.
        let own_dir = match f.file_name().and_then(|n| n.to_str()) {
            Some("main.rs" | "lib.rs" | "mod.rs") => parent.to_path_buf(),
            _ => parent.join(f.file_stem().unwrap_or_default()),
        };
        for (i, line) in lines.iter().enumerate() {
            let Some(rest) = line.strip_prefix("#[cfg(test)]") else {
                continue;
            };
            let mut path = None;
            let after = std::iter::once(rest.trim()).chain(lines[i + 1..].iter().copied());
            for next in after {
                if let Some(c) = path_attr.captures(next) {
                    path = Some(c[1].to_string());
                    continue;
                }
                if next.is_empty() || next.starts_with("#[") {
                    continue;
                }
                if let Some(c) = decl.captures(next) {
                    let file = match &path {
                        Some(p) => parent.join(p),
                        None => own_dir.join(format!("{}.rs", &c[1])),
                    };
                    out.push(file.with_extension(""));
                    out.push(file);
                }
                break;
            }
        }
    }
    out
}

/// `f` is a test-only module file, or lives under one.
fn is_test_file(tests: &[PathBuf], f: &Path) -> bool {
    tests.iter().any(|t| f.starts_with(t))
}

fn mount_re() -> Regex {
    Regex::new(r#"machine_gate::mount\(\s*[^,"]+?\s*,\s*"([^"]+)"\s*,\s*&\[([^\]]*)\]"#).unwrap()
}

/// Every mount in the tree: service name → (file, exempt paths).
fn mounts(root: &Path) -> BTreeMap<String, (String, Vec<String>)> {
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let re = mount_re();
    let lit = Regex::new(r#""([^"]+)""#).unwrap();
    let tests = test_only_modules(&files);
    let mut out = BTreeMap::new();
    for f in files.iter().filter(|f| !is_test_file(&tests, f)) {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().to_string();
        for c in re.captures_iter(&production_part(&text)) {
            let exempt = lit.captures_iter(&c[2]).map(|l| l[1].to_string()).collect();
            let prior = out.insert(c[1].to_string(), (rel.clone(), exempt));
            assert!(
                prior.is_none(),
                "service `{}` is mounted twice ({} and {rel}); one service, one gate",
                &c[1],
                prior.unwrap().0
            );
        }
    }
    out
}

#[test]
fn every_server_outside_a_test_mounts_the_machine_gate() {
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let tests = test_only_modules(&files);
    let mut servers = 0;
    let mut offenders = Vec::new();
    let mut ungated_seen = Vec::new();
    let private = private_listeners(&root);
    let mut private_seen = vec![0; private.len()];
    for f in files.iter().filter(|f| !is_test_file(&tests, f)) {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let prod = production_part(&text);
        let serves = server_calls(&rel, &prod);
        if serves == 0 {
            continue;
        }
        servers += 1;
        if rel == ROLE_SERVER_ADAPTER {
            continue;
        }
        for server in server_gate_scan::scan(&prod).expect("server inventory parses") {
            if let Some(row) = UNGATED.iter().find(|u| u.file == rel) {
                ungated_seen.push(row.file);
            } else if let Some((index, row)) = private
                .iter()
                .enumerate()
                .find(|(_, row)| row.file == rel && row.function == server.function)
            {
                assert_eq!(
                    server.kind, "unaccounted TCP listener",
                    "a private declaration cannot excuse another adapter: {}",
                    row.why
                );
                assert_eq!(row.bound, "loopback");
                private_seen[index] += 1;
            } else if !server.gated {
                offenders.push(format!(
                    "{rel}: {}: {} has no proven mounted served app",
                    server.function, server.kind
                ));
            }
        }
    }
    // A control: the scan found the servers it exists to judge. 27
    // binaries served HTTP when this landed; far fewer means the walk
    // or the test-region cut is broken, not that the servers left.
    assert!(
        servers >= 25,
        "found only {servers} production servers under crates/ — the scan is broken"
    );
    assert!(
        offenders.is_empty(),
        "an HTTP server that does not mount boss_core::machine_gate cannot be moved to \
         report or enforce (design 6805c764). Serve through \
         `boss_core::machine_gate::mount(app, \"<boss-ports name>\", &[<exact health paths>])`, \
         or add the file to UNGATED with the reason:\n  {}",
        offenders.join("\n  ")
    );
    for row in UNGATED {
        assert!(
            ungated_seen.contains(&row.file),
            "UNGATED names {}, which no longer serves outside its tests — delete the row",
            row.file
        );
    }
    assert!(
        private_seen.iter().all(|count| *count == 1),
        "each private function declaration must account for exactly one listener"
    );
}

/// A module file whose parent declares it only under `#[cfg(test)]` is
/// a test's fixture, as much as the region after a `#[cfg(test)]`
/// inside a file is. Car a159e1ee's dock re-gate went red on exactly
/// this shape through five trains (2026-09-25): the gateway's
/// `a_read_only_session_cannot_write.rs` and
/// `a_path_cannot_climb_out_of_its_route.rs` each serve a recording
/// upstream, and `main.rs` declares both as `#[cfg(test)] mod …;`.
/// The control — a plain `mod` beside them — stays production, so the
/// cut cannot swallow a real server.
#[test]
fn a_module_declared_only_under_cfg_test_is_a_test_file() {
    let root = boss_testing::scratch_dir("machine-gate-test-modules");
    let src = root.join("crates/core/boss-demo/src");
    boss_testing::create_dir(&src.join("nested"));
    boss_testing::write_file(
        &src.join("main.rs"),
        "#[cfg(test)]\nmod fixture_server;\n#[cfg(test)]\n#[path = \"elsewhere.rs\"]\nmod renamed;\n\
         #[cfg(test)]\nmod nested;\nmod real_server;\n#[cfg(test)]\nmod tests {}\n",
    );
    for f in [
        "fixture_server.rs",
        "elsewhere.rs",
        "real_server.rs",
        "nested/mod.rs",
    ] {
        boss_testing::write_file(&src.join(f), "mod child;\n");
    }
    boss_testing::write_file(&src.join("nested/child.rs"), "");
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let tests = test_only_modules(&files);
    let judged = |rel: &str| is_test_file(&tests, &src.join(rel));
    assert!(
        judged("fixture_server.rs"),
        "a #[cfg(test)] mod is a test file"
    );
    assert!(
        judged("elsewhere.rs"),
        "a #[cfg(test)] mod behind #[path] is a test file"
    );
    assert!(
        judged("nested/mod.rs"),
        "a #[cfg(test)] mod in a directory is a test file"
    );
    assert!(
        judged("nested/child.rs"),
        "a test module's children are test files"
    );
    assert!(
        !judged("real_server.rs"),
        "THE CONTROL: a plain mod stays production"
    );
    assert!(
        !judged("main.rs"),
        "the declaring file itself stays production"
    );
}

/// The test region of a file starts at its inline `#[cfg(test)] mod …
/// {`, not at the first `#[cfg(test)]` it carries. The gateway's
/// `main.rs` opens with two `#[cfg(test)] mod …;` declarations since
/// 2026-09-25, and cutting there left none of it production: its
/// `axum::serve` vanished from the scan and the UNGATED row read stale.
#[test]
fn a_cfg_test_module_declaration_does_not_end_the_production_part() {
    let text = "#[cfg(test)]\nmod fixture;\n#[cfg(test)] mod other;\n\
                async fn main() { axum::serve(listener, app).await; }\n\
                #[cfg(test)]\n#[allow(dead_code)]\nmod tests {\n    fn f() { axum::serve(l, a); }\n}\n";
    let prod = production_part(text);
    assert_eq!(prod.matches("axum::serve(").count(), 1, "{prod}");
    assert!(!prod.contains("mod tests"), "{prod}");
    assert_eq!(production_part("fn main() {}\n"), "fn main() {}\n");
}

/// The gated servers that hand their gate NO recorder, each with why:
/// what they tally reaches no log, so a clean window that names them is
/// never clean (design 21946380 point 7). Every other mount passes one.
const NO_RECORDER: [(&str, &str); 2] = [
    (
        "sim-control",
        "the brewery engine's control server holds no database, so it has no outbox",
    ),
    (
        "simulator",
        "the /simulator UX server holds no database, so it has no outbox",
    ),
];

/// Every gated server states what it would refuse on the LOG (design
/// 21946380, backlog b0787727): its mount's recorder is a value, or the
/// server is named in [`NO_RECORDER`] with the reason. A mount added
/// with `None` and no reason is a service whose clean window can never
/// be read — the hole this pin exists to name before an enforce flip
/// finds it. A stale row (the mount now records) fails too.
#[test]
fn every_mount_states_its_facts_on_the_log_or_says_why_not() {
    let root = repo_root();
    let none = Regex::new(
        r#"machine_gate::mount\(\s*[^,"]+?\s*,\s*"([^"]+)"\s*,\s*&\[[^\]]*\]\s*,\s*None\s*,?\s*\)"#,
    )
    .unwrap();
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let tests = test_only_modules(&files);
    let mut unrecorded: Vec<String> = files
        .iter()
        .filter(|f| !is_test_file(&tests, f))
        .flat_map(|f| {
            let text = std::fs::read_to_string(f).unwrap_or_default();
            none.captures_iter(&production_part(&text))
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    unrecorded.sort();
    let mut excused: Vec<String> = NO_RECORDER.iter().map(|(s, _)| s.to_string()).collect();
    excused.sort();
    assert_eq!(
        unrecorded, excused,
        "a machine gate mounted with no recorder states nothing on the log, so its service's \
         clean window can never be read (design 21946380). Pass the outbox recorder of the \
         binary's pool (`boss_events::outbox`), or name the service in NO_RECORDER with the \
         reason"
    );
    // A control: the scan sees the mounts it judges.
    assert!(mounts(&root).len() >= 25);
}

/// The gate's evidence owns SIGTERM in every binary that records it
/// (`boss_core::gate_evidence`): it drains, states the process's end and
/// exits, because registering a listener replaces the signal's default
/// for the whole process. A gated crate that registered its own SIGTERM
/// shutdown would be cut short by that exit within milliseconds, in
/// silence (review c49cb4e1, S2) — so it is refused here, naming the
/// module to change first. No gated crate does today: jobs, assets and
/// ledger shut down on SIGINT.
#[test]
fn a_gated_binary_does_not_listen_for_sigterm() {
    let root = repo_root();
    let sigterm = Regex::new(r"SignalKind::terminate\b|\bSIGTERM\b\s*[,)]").unwrap();
    // The control: the one owner is seen by the same pattern.
    let owner = std::fs::read_to_string(root.join("crates/core/boss-core/src/gate_evidence.rs"))
        .expect("gate_evidence.rs is readable");
    assert!(
        sigterm.is_match(&owner),
        "the scan cannot see the evidence's own listener"
    );
    let crates: std::collections::BTreeSet<PathBuf> = mounts(&root)
        .values()
        .map(|(file, _)| Path::new(file).components().take(3).collect::<PathBuf>())
        .collect();
    assert!(
        crates.len() >= 20,
        "found only {} gated crates",
        crates.len()
    );
    let mut offenders = Vec::new();
    for dir in &crates {
        let mut files = Vec::new();
        walk(&root.join(dir).join("src"), &mut files);
        let tests = test_only_modules(&files);
        for f in files.iter().filter(|f| !is_test_file(&tests, f)) {
            let text = std::fs::read_to_string(f).unwrap_or_default();
            if sigterm.is_match(&production_part(&text)) {
                offenders.push(f.strip_prefix(&root).unwrap().display().to_string());
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a gated crate registers its own SIGTERM, which boss_core::gate_evidence already owns \
         and exits on once its end is stated — the crate's shutdown would be cut short in \
         silence. Make the end something this binary awaits from its own shutdown path in \
         gate_evidence first (review c49cb4e1, S2):\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn the_shared_role_shutdown_leaves_sigterm_to_gate_evidence() {
    let source = std::fs::read_to_string(
        repo_root().join("crates/core/boss-policy-client/src/role_service.rs"),
    )
    .unwrap();
    let sigterm = Regex::new(r"SignalKind::terminate\b|\bSIGTERM\b\s*[,)]").unwrap();
    assert!(sigterm.is_match("signal(SignalKind::terminate())"));
    assert!(!sigterm.is_match("tokio::signal::ctrl_c().await"));
    assert!(
        !sigterm.is_match(&production_part(&source)),
        "the shared role-report shutdown registers a second process termination owner"
    );
    assert!(source.contains("tokio::signal::ctrl_c().await"));
}

#[test]
fn every_mount_names_a_port_roster_service() {
    let root = repo_root();
    let m = mounts(&root);
    assert!(m.len() >= 25, "found only {} mounts", m.len());
    let names: Vec<&str> = boss_ports::all().map(|s| s.name).collect();
    let unknown: Vec<String> = m
        .iter()
        .filter(|(svc, _)| !names.contains(&svc.as_str()))
        .map(|(svc, (file, _))| format!("{file}: `{svc}`"))
        .collect();
    assert!(
        unknown.is_empty(),
        "a mount names a service boss-ports does not know, so no probe can be matched to it:\n  {}",
        unknown.join("\n  ")
    );
    for path in m.values().flat_map(|(_, e)| e) {
        assert!(
            path.starts_with('/') && !path.ends_with('*'),
            "exemptions are exact paths (design 6805c764 choice 5), not `{path}`"
        );
    }
}

/// `(manifest, path, port)` for every httpGet probe in the manifests.
fn probes(root: &Path) -> Vec<(String, String, String)> {
    let dir = root.join("infra/cluster/manifests");
    let flow = Regex::new(r"httpGet:\s*\{\s*path:\s*([^,}\s]+)\s*,\s*port:\s*([^,}\s]+)").unwrap();
    let block_path = Regex::new(r"^\s*path:\s*(\S+)").unwrap();
    let block_port = Regex::new(r"^\s*port:\s*(\S+)").unwrap();
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for f in entries {
        if !f.extension().is_some_and(|x| x == "yaml" || x == "yml") {
            continue;
        }
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&f).unwrap();
        for c in flow.captures_iter(&text) {
            out.push((name.clone(), c[1].to_string(), c[2].to_string()));
        }
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.trim() != "httpGet:" {
                continue;
            }
            let window = &lines[i + 1..lines.len().min(i + 4)];
            let path = window
                .iter()
                .find_map(|w| block_path.captures(w).map(|c| c[1].to_string()));
            let port = window
                .iter()
                .find_map(|w| block_port.captures(w).map(|c| c[1].to_string()));
            match (path, port) {
                (Some(p), Some(q)) => out.push((name.clone(), p, q)),
                _ => panic!("{name}:{}: an httpGet probe this pin cannot read", i + 1),
            }
        }
    }
    out
}

fn port_number(root: &Path, manifest: &str, port: &str) -> u16 {
    if let Ok(n) = port.parse() {
        return n;
    }
    let text =
        std::fs::read_to_string(root.join("infra/cluster/manifests").join(manifest)).unwrap();
    let re = Regex::new(&format!(
        r"containerPort:\s*(\d+)\s*,\s*name:\s*{}\b",
        regex::escape(port)
    ))
    .unwrap();
    re.captures(&text)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or_else(|| panic!("{manifest}: probe port `{port}` names no containerPort"))
}

#[test]
fn every_probe_and_the_watchdog_read_are_exempt_where_they_land() {
    let root = repo_root();
    let m = mounts(&root);
    let mut needs: Vec<(String, String, String)> = Vec::new(); // (service, path, from)

    let found = probes(&root);
    assert!(
        !found.is_empty(),
        "no httpGet probe found in infra/cluster/manifests — the gateway's readiness probe \
         existed when this landed, so the parse is broken"
    );
    for (manifest, path, port) in found {
        let n = port_number(&root, &manifest, &port);
        if let Some(spec) = boss_ports::all().find(|s| s.prod == n || s.scratch == Some(n)) {
            needs.push((
                spec.name.to_string(),
                path,
                format!("{manifest} httpGet port {port}"),
            ));
        }
    }

    let watchdog = "infra/forge/cluster-watchdog.sh";
    let text = std::fs::read_to_string(root.join(watchdog)).unwrap();
    let re = Regex::new(r"\$\{?JOBS_API\}?(/[A-Za-z0-9/_.-]+)").unwrap();
    let reads: Vec<String> = re.captures_iter(&text).map(|c| c[1].to_string()).collect();
    assert!(
        !reads.is_empty(),
        "{watchdog} reads no $JOBS_API path — it read /api/jobs/health when this landed, so \
         the parse is broken"
    );
    for path in reads {
        needs.push(("jobs".to_string(), path, watchdog.to_string()));
    }

    // The estate observer's dead-letter read (backlog 37742794,
    // 2026-10-06). It is the same class as the watchdog's: an alarm's
    // read, sent with no identity ON PURPOSE — boss-dispatcher's http.rs
    // keeps `readyz` open "by decision rather than oversight", because
    // an alarm that must be admitted before it can speak is an arm that
    // needs the patient. The dispatcher's mount exempted `health` alone,
    // so the gate tallied the read as a would-refuse fact every fifteen
    // minutes (281 in the 72 hours to 2026-10-06T17:41Z, from 207 pod
    // addresses) and would have answered it 401 under enforce — the
    // observer's `dispatcher_unread`, three of which raise an alarm that
    // says the dispatcher went blind when the gate did.
    let observer = "infra/cluster/manifests/boss-estate-observe.yaml";
    let text = std::fs::read_to_string(root.join(observer)).unwrap();
    let re = Regex::new(r#"curl [^\n]*"\$\{?DISPATCHER_API\}?(/[A-Za-z0-9/_.-]+)""#).unwrap();
    let reads: Vec<String> = re.captures_iter(&text).map(|c| c[1].to_string()).collect();
    assert!(
        !reads.is_empty(),
        "{observer} reads no $DISPATCHER_API path — it read /api/dispatcher/readyz when this \
         landed, so the parse is broken"
    );
    for path in reads {
        needs.push(("dispatcher".to_string(), path, observer.to_string()));
    }

    let mut missing = Vec::new();
    for (svc, path, from) in needs {
        if UNGATED.iter().any(|u| u.service == Some(svc.as_str())) {
            continue;
        }
        match m.get(&svc) {
            Some((_, exempt)) if exempt.contains(&path) => {}
            Some((file, _)) => missing.push(format!(
                "{from} reads {path} on `{svc}`, which {file} does not exempt"
            )),
            None => missing.push(format!(
                "{from} reads {path} on `{svc}`, and no machine_gate::mount names `{svc}`"
            )),
        }
    }
    assert!(
        missing.is_empty(),
        "a reader that holds no token would be refused once the mode leaves `off` \
         (design 6805c764 choice 5):\n  {}",
        missing.join("\n  ")
    );
}
