//! tree-wide pin — timing coverage follows the actual owner route registrations.
use boss_core::request_latency::Class;
use std::collections::BTreeSet;
use std::path::Path;
use syn::visit::Visit;

type Routes = BTreeSet<(String, String)>;

#[derive(Default)]
struct Methods(BTreeSet<String>);
impl<'ast> Visit<'ast> for Methods {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*call.func
            && let Some(name) = path.path.segments.last()
        {
            self.0.insert(name.ident.to_string().to_uppercase());
        }
        syn::visit::visit_expr_call(self, call);
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.0.insert(call.method.to_string().to_uppercase());
        syn::visit::visit_expr_method_call(self, call);
    }
}

#[derive(Default)]
struct Inventory(Routes);
impl<'ast> Visit<'ast> for Inventory {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "route" && call.args.len() == 2 {
            let mut methods = Methods::default();
            methods.visit_expr(&call.args[1]);
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(path),
                ..
            }) = &call.args[0]
            {
                for method in methods.0 {
                    if ["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str()) {
                        self.0.insert((method, path.value()));
                    }
                }
            } else {
                assert!(
                    !methods
                        .0
                        .iter()
                        .any(|method| ["POST", "PUT", "PATCH"].contains(&method.as_str())),
                    "a nonliteral owner write route requires explicit inventory resolution; never assume it is outside timing scope"
                );
            }
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

fn routes(source: &str) -> Routes {
    let source = boss_testing::production_source::production_text(source).unwrap();
    let file = syn::parse_file(&source).unwrap();
    let mut inventory = Inventory::default();
    inventory.visit_file(&file);
    inventory.0
}

fn walk(path: &Path, found: &mut Routes) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.extend(routes(&std::fs::read_to_string(path).unwrap()));
        }
    }
}

// These owner routes control simulation/StepPlugin registry data, not Job or Step rows.
// Each exclusion must remain a real registered route; a new write gets no implicit exclusion.
const NON_PACKET_WRITES: &[(&str, &str, &str)] = &[
    (
        "POST",
        "/api/jobs/sim-clock/pause",
        "simulation clock pause",
    ),
    (
        "POST",
        "/api/jobs/sim-clock/resume",
        "simulation clock resume",
    ),
    (
        "POST",
        "/api/jobs/sim-clock/restart-epoch",
        "simulation clock epoch",
    ),
    (
        "POST",
        "/api/jobs/step-plugins",
        "author supporting registry version",
    ),
    (
        "PUT",
        "/api/jobs/step-plugins/{kind}",
        "update supporting registry draft",
    ),
    (
        "POST",
        "/api/jobs/step-plugins/{kind}/publish",
        "publish supporting registry version",
    ),
    (
        "POST",
        "/api/jobs/step-plugins/{kind}/retire",
        "retire supporting registry version",
    ),
];

fn missing(routes: &Routes, classify: impl Fn(&str, &str) -> Option<Class>) -> Vec<String> {
    routes
        .iter()
        .filter(|(method, path)| {
            path.starts_with("/api/jobs")
                && ["POST", "PUT", "PATCH"].contains(&method.as_str())
                && !NON_PACKET_WRITES
                    .iter()
                    .any(|(excluded_method, excluded_path, _)| {
                        method == excluded_method && path == excluded_path
                    })
                && !matches!(classify(method, path), Some(Class::Write | Class::Bulk))
        })
        .map(|(method, path)| format!("{method} {path}"))
        .collect()
}

#[test]
fn registered_job_and_step_writes_cannot_silently_escape_timing() {
    let mut actual = Routes::new();
    walk(
        &boss_testing::repo_root().join("crates/core/boss-jobs/src"),
        &mut actual,
    );
    for (method, path, why) in NON_PACKET_WRITES {
        assert!(
            actual.contains(&((*method).into(), (*path).into())),
            "stale non-packet exclusion: {method} {path}: {why}"
        );
    }
    assert!(
        actual
            .iter()
            .filter(|(_, path)| path.starts_with("/api/jobs/{id}/"))
            .count()
            >= 15,
        "owner route inventory unexpectedly empty"
    );
    assert!(actual.contains(&("GET".into(), "/api/jobs/{id}".into())));
    let uncovered = missing(&actual, boss_jobs::sensors::request_latency::classify);
    assert!(
        uncovered.is_empty(),
        "eligible registered routes lack timing: {uncovered:?}"
    );
    assert_eq!(
        boss_jobs::sensors::request_latency::classify("GET", "/api/sensors/request-latency"),
        None
    );
    let mutant = missing(&actual, |method, path| {
        if path.ends_with("/complete-if") {
            None
        } else {
            boss_jobs::sensors::request_latency::classify(method, path)
        }
    });
    assert!(
        mutant.iter().any(|route| route.ends_with("/complete-if")),
        "layered conditional command must be inventoried by name"
    );
}

#[test]
fn a_future_unclassified_write_is_named_instead_of_assumed_covered() {
    let future =
        routes("fn app(){ Router::new().route(\"/api/jobs/batch\", get(read).post(create)); }");
    assert_eq!(
        missing(&future, boss_jobs::sensors::request_latency::classify),
        ["POST /api/jobs/batch"]
    );
}

#[test]
#[should_panic(expected = "nonliteral owner write route")]
fn an_opaque_new_write_cannot_silently_escape_the_inventory() {
    routes("fn app(){ Router::new().route(NEW_ROUTE, post(create)); }");
}
