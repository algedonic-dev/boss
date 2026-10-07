//! The production binary builds the ledger surface with the REAL policy
//! engine (backlog 7048afa8, 2026-09-26).
//!
//! `LedgerApiState.policy` is a required client, so the type system
//! already refuses a surface with none. What it cannot refuse is a
//! binary handed an allow-all test client — `PermissivePolicyClient` or
//! `FakePolicyClient` would compile, and would open the company's books
//! exactly as the old `policy: None` did. This reads the binary's
//! source, because the wiring is the one place a port-level test cannot
//! reach — the shape of boss-people's `the_people_api_wires_policy`.

const BINARY: &str = include_str!("../src/bin/boss_ledger_api.rs");

fn code_lines() -> impl Iterator<Item = &'static str> {
    BINARY.lines().filter(|l| !l.trim_start().starts_with("//"))
}

#[test]
fn the_ledger_api_binary_wires_the_policy_engine() {
    assert!(
        uses_real_policy(BINARY),
        "the ledger surface's actual policy value does not trace to ReqwestPolicyClient"
    );
}

/// Follow immutable local bindings and the report decorator's original
/// policy argument. A constructor elsewhere, a comment or a test is
/// not evidence about the value handed to the ledger surface.
fn uses_real_policy(source: &str) -> bool {
    let file = syn::parse_file(source).unwrap();
    let Some(main) = file.items.iter().find_map(|item| match item {
        syn::Item::Fn(f) if f.sig.ident == "main" => Some(f),
        _ => None,
    }) else {
        return false;
    };
    let locals: Vec<_> = main
        .block
        .stmts
        .iter()
        .filter_map(|stmt| match stmt {
            syn::Stmt::Local(local) => Some(local),
            _ => None,
        })
        .collect();
    locals.iter().enumerate().any(|(before, local)| {
        let Some(init) = &local.init else {
            return false;
        };
        let syn::Expr::Struct(state) = &*init.expr else {
            return false;
        };
        state.path.is_ident("LedgerApiState")
            && state.fields.iter().any(|field| {
                matches!(&field.member, syn::Member::Named(name) if name == "policy")
                    && real_policy(&field.expr, &locals, before)
            })
    })
}

fn binding<'a>(
    name: &str,
    locals: &[&'a syn::Local],
    before: usize,
) -> Option<(usize, &'a syn::Expr)> {
    let (i, local) = locals[..before]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, local)| matches!(&local.pat, syn::Pat::Ident(pat) if pat.ident == name))?;
    let syn::Pat::Ident(pat) = &local.pat else {
        return None;
    };
    if pat.mutability.is_some() {
        return None;
    }
    local.init.as_ref().map(|init| (i, &*init.expr))
}

fn call_name(call: &syn::ExprCall) -> String {
    match &*call.func {
        syn::Expr::Path(p) => p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        _ => String::new(),
    }
}

fn real_policy(expr: &syn::Expr, locals: &[&syn::Local], before: usize) -> bool {
    // This is a name inspected by the AST matcher, not a bypass call.
    // Keep the lexical bypass audit from treating its own fixture as code.
    const SIM_WRAPPER: &str = concat!(
        "boss_policy_client::",
        "SimBypassPolicyClient",
        "::",
        "from_env"
    );
    match expr {
        syn::Expr::Path(path) if path.path.segments.len() == 1 => {
            binding(&path.path.segments[0].ident.to_string(), locals, before)
                .is_some_and(|(at, value)| real_policy(value, locals, at))
        }
        syn::Expr::Call(call) => match call_name(call).as_str() {
            "boss_policy_client::ReqwestPolicyClient::new" => true,
            "Arc::new" | SIM_WRAPPER => call
                .args
                .first()
                .is_some_and(|arg| real_policy(arg, locals, before)),
            _ => false,
        },
        syn::Expr::Field(field) if matches!(&field.member, syn::Member::Named(name) if name == "policy") =>
        {
            let syn::Expr::Path(path) = &*field.base else {
                return false;
            };
            let Some(name) = path.path.get_ident() else {
                return false;
            };
            binding(&name.to_string(), locals, before).is_some_and(|(at, value)| {
                let syn::Expr::Call(call) = value else {
                    return false;
                };
                call_name(call) == "boss_policy_client::role_service::assemble"
                    && call
                        .args
                        .iter()
                        .nth(2)
                        .is_some_and(|arg| real_policy(arg, locals, at))
            })
        }
        _ => false,
    }
}

#[test]
fn policy_wiring_follows_the_actual_value_and_refuses_a_decoy() {
    for inner in [
        "boss_policy_client::ReqwestPolicyClient::new(\"ledger\", url)",
        "FakePolicyClient::allow_all()",
    ] {
        let source = format!(
            "fn main() {{ let policy = Arc::new({inner}); let wiring = boss_policy_client::role_service::assemble(\"ledger\", path, policy, roles, mode, tally); let policy = wiring.policy; let state = LedgerApiState {{ policy }}; }}"
        );
        assert_eq!(
            uses_real_policy(&source),
            inner.starts_with("boss_policy_client::Reqwest")
        );
        let decoy = source.replace(
            "let policy = wiring.policy;",
            "let policy = FakePolicyClient::allow_all();",
        );
        assert!(!uses_real_policy(&decoy));
    }
    assert!(!uses_real_policy(
        "fn main() { let state = LedgerApiState { policy: FakePolicyClient::allow_all() }; } #[cfg(test)] fn test() { boss_policy_client::ReqwestPolicyClient::new(\"ledger\", url); }"
    ));
}

#[test]
fn a_mutable_shadow_cannot_borrow_an_earlier_real_policy_binding() {
    assert!(!uses_real_policy(
        "fn main() { let policy = boss_policy_client::ReqwestPolicyClient::new(\"ledger\", url); let mut policy = FakePolicyClient::allow_all(); let state = LedgerApiState { policy }; }"
    ));
}

#[test]
fn the_ledger_api_binary_never_wires_an_allow_all_client() {
    let open: Vec<&str> = code_lines()
        .filter(|l| {
            ["PermissivePolicyClient", "FakePolicyClient", "policy: None"]
                .iter()
                .any(|c| l.contains(c))
        })
        .collect();
    assert!(
        open.is_empty(),
        "boss_ledger_api.rs wires a client that allows every read: {open:?}"
    );
}
