//! Structural coverage pin, not a substitute for the owners' HTTP controls.
//! A new port row must name a production snapshot/refresh owner and report
//! route. Special composition lives with classes/policy; the session edge
//! remains trusted and the simulator daemon has no durable window.
use std::path::{Path, PathBuf};

fn owner(root: &Path, service: &str) -> PathBuf {
    let special = match service {
        "gateway" => Some("crates/core/boss-gateway/src/main.rs"),
        "ml" => Some("crates/orchestrators/boss-ml-api/src/main.rs"),
        "events" => Some("crates/orchestrators/boss-events-api/src/main.rs"),
        "dispatcher" => {
            Some("crates/orchestrators/boss-dispatcher-handlers/src/bin/boss_dispatcher.rs")
        }
        "simulator" => Some("crates/orchestrators/boss-simulator/src/bin/boss_simulator.rs"),
        "sim-control" => Some("crates/tenants/boss-brewery-engine/src/sim_control.rs"),
        _ => None,
    };
    if let Some(path) = special {
        return root.join(path);
    }
    let binary = service.replace('-', "_");
    let candidates: Vec<_> = ["core", "modules"]
        .iter()
        .map(|tier| {
            root.join(format!(
                "crates/{tier}/boss-{service}/src/bin/boss_{binary}_api.rs"
            ))
        })
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "{service}: exactly one HTTP owner required"
    );
    candidates[0].clone()
}

#[derive(Default)]
struct Production {
    paths: Vec<String>,
    strings: std::collections::BTreeSet<String>,
}

fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Path>()
                .is_ok_and(|path| path.is_ident("test"))
    })
}

impl<'ast> syn::visit::Visit<'ast> for Production {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !test_only(&item.attrs) {
            syn::visit::visit_item_fn(self, item);
        }
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !test_only(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.paths.push(
            path.segments
                .iter()
                .map(|part| part.ident.to_string())
                .collect::<Vec<_>>()
                .join("::"),
        );
        syn::visit::visit_path(self, path);
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.paths.push(call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
    }
    fn visit_lit_str(&mut self, value: &'ast syn::LitStr) {
        self.strings.insert(value.value());
    }
}

impl Production {
    fn has(&self, path: &str) -> bool {
        self.paths
            .iter()
            .any(|actual| actual == path || actual.ends_with(&format!("::{path}")))
    }
}

fn production(path: &Path) -> Production {
    use syn::visit::Visit;
    let source =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let ast =
        syn::parse_file(&source).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut result = Production::default();
    result.visit_file(&ast);
    result
}

#[test]
fn every_registered_service_has_a_production_role_report_owner() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap();
    let services: Vec<_> = boss_ports::all().collect();
    assert!(
        !services.is_empty(),
        "empty service registry cannot prove coverage"
    );
    for service in services {
        let path = owner(root, service.name);
        let source = production(&path);
        for marker in [
            "SnapshotRoleReader::new",
            "MountedReportMode::mount",
            "HttpRoleReader::new",
        ] {
            assert!(
                source.has(marker),
                "{}: {} lacks {marker}",
                service.name,
                path.display()
            );
        }
        assert!(
            source.has("serve_with_refresh") || source.has("run_refresh_loop"),
            "{}: no production refresh lifetime",
            service.name
        );
        let route = match service.name {
            "sim-control" => "/actor-role-reports".to_owned(),
            "simulator" => "/simulator/api/actor-role-reports".to_owned(),
            name => format!("/api/{name}/actor-role-reports"),
        };
        let mount = match service.name {
            "classes" | "policy" => production(
                &path
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("role_reports.rs"),
            ),
            _ => source,
        };
        assert!(
            mount.strings.contains(&route),
            "{}: production report route {route} absent",
            service.name
        );
    }
}
