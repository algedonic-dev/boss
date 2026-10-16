mod server_gate_scan;

#[test]
fn wildcard_imports_and_nested_methods_cannot_borrow_outer_identity() {
    mod real {
        pub fn mount(app: u8) -> u8 {
            app + 1
        }
    }
    mod fake {
        pub fn mount(app: u8) -> u8 {
            app
        }
    }
    use real::mount;
    assert_eq!(mount(7), 8);
    {
        use fake::*;
        assert_eq!(mount(7), 7);
    }
    trait Mount {
        fn mount(app: u8) -> u8;
    }
    struct Identity;
    impl Mount for Identity {
        fn mount(app: u8) -> u8 {
            app
        }
    }
    fn nested() -> u8 {
        struct S<T>(std::marker::PhantomData<T>);
        impl<Gate: Mount> S<Gate> {
            fn run(app: u8) -> u8 {
                Gate::mount(app)
            }
        }
        trait T<Gate: Mount> {
            fn run(app: u8) -> u8 {
                Gate::mount(app)
            }
        }
        impl T<Identity> for S<Identity> {}
        assert_eq!(<S<Identity> as T<Identity>>::run(7), 7);
        S::<Identity>::run(7)
    }
    assert_eq!(nested(), 7);
    let cases = [
        "use boss_core::machine_gate::mount; fn main(app:App,listener:Listener){use fake::*; let app=mount(app); axum::serve(listener,app);}",
        "use boss_core::machine_gate as Gate; fn outer(){struct S<T>(std::marker::PhantomData<T>); impl<Gate:Mount> S<Gate> {fn run(app:App,listener:Listener){let app=Gate::mount(app); axum::serve(listener,app);}}}",
        "use boss_core::machine_gate as Gate; trait T<Gate>{fn run(app:App,listener:Listener){let app=Gate::mount(app); axum::serve(listener,app);}}",
    ];
    let mut failures = Vec::new();
    for source in cases {
        let rows = server_gate_scan::scan(source).unwrap();
        if rows.len() != 1 || rows[0].gated {
            failures.push(source);
        }
    }
    assert!(
        failures.is_empty(),
        "unproved namespace accepted or omitted: {failures:?}"
    );
}

#[test]
fn local_type_shadows_an_imported_module_namespace() {
    mod lexical {
        mod original {
            pub fn mount(app: u8) -> u8 {
                app + 1
            }
        }
        use original as Gate;
        pub fn observed() -> (u8, u8) {
            struct Gate;
            impl Gate {
                fn mount(app: u8) -> u8 {
                    app
                }
            }
            (self::Gate::mount(7), Gate::mount(7))
        }
    }
    assert_eq!(lexical::observed(), (8, 7));
    let source = "use boss_core::machine_gate as Gate; fn main(app: App, listener: Listener) { struct Gate; impl Gate { fn mount<T>(app: T) -> T { app } } let app = Gate::mount(app); axum::serve(listener, app); }";
    let servers = server_gate_scan::scan(source).unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].function, "main");
    assert_eq!(servers[0].kind, "HTTP serve");
    assert!(
        !servers[0].gated,
        "local type identity mount was resolved as the imported gate module"
    );
}

#[test]
fn namespace_inventory_measures_declarations_and_keeps_real_import_positives() {
    mod lexical {
        mod original {
            pub fn mount(app: u8) -> u8 {
                app + 1
            }
        }
        use original as Gate;
        use original::mount as gate;
        trait IdentityMount {
            fn mount(app: u8) -> u8;
        }
        struct Identity;
        impl IdentityMount for Identity {
            fn mount(app: u8) -> u8 {
                app
            }
        }
        pub fn observed() -> Vec<u8> {
            let imported = self::Gate::mount(7);
            let enumeration = {
                enum Gate {}
                impl Gate {
                    fn mount(app: u8) -> u8 {
                        app
                    }
                }
                Gate::mount(7)
            };
            let union = {
                union Gate {
                    _value: u8,
                }
                impl Gate {
                    fn mount(app: u8) -> u8 {
                        app
                    }
                }
                let _ = Gate { _value: 1 };
                Gate::mount(7)
            };
            let alias = {
                type Gate = Identity;
                Gate::mount(7)
            };
            let association = {
                trait Gate {
                    fn mount(app: Self) -> Self;
                }
                impl Gate for u8 {
                    fn mount(app: Self) -> Self {
                        app
                    }
                }
                Gate::mount(7u8)
            };
            fn generic<Gate: IdentityMount>() -> u8 {
                Gate::mount(7)
            }
            struct Wrapper<Gate>(std::marker::PhantomData<Gate>);
            impl<Gate: IdentityMount> Wrapper<Gate> {
                fn observe() -> u8 {
                    Gate::mount(7)
                }
            }
            vec![
                imported,
                enumeration,
                union,
                alias,
                association,
                generic::<Identity>(),
                Wrapper::<Identity>::observe(),
            ]
        }
        pub fn imports_and_namespaces() -> Vec<u8> {
            use self::original as gate_type;
            let (outer_type, inner_type) = {
                mod gate_type {
                    pub fn mount(app: u8) -> u8 {
                        app
                    }
                }
                let outer = gate_type::mount(7);
                let inner = {
                    use self::original as gate_type;
                    gate_type::mount(7)
                };
                (outer, inner)
            };
            let (outer_value, inner_value) = {
                fn gate(app: u8) -> u8 {
                    app
                }
                let outer = gate(7);
                let inner = {
                    use self::original::mount as gate;
                    gate(7)
                };
                (outer, inner)
            };
            let separate_namespace = {
                mod gate {}
                gate(7)
            };
            assert_eq!(gate_type::mount(7), 8);
            vec![
                outer_type,
                inner_type,
                outer_value,
                inner_value,
                separate_namespace,
            ]
        }
    }
    assert_eq!(lexical::observed(), vec![8, 7, 7, 7, 7, 7, 7]);
    assert_eq!(lexical::imports_and_namespaces(), vec![7, 8, 7, 8, 8]);
    let mut mismatches = Vec::new();
    for (name, source, expected) in [
        (
            "struct",
            "use boss_core::machine_gate as Gate; fn main(app: App, listener: Listener) { struct Gate; impl Gate { fn mount<T>(app:T)->T {app} } let app=Gate::mount(app); axum::serve(listener,app); }",
            false,
        ),
        (
            "enum",
            "use boss_core::machine_gate as Gate; fn main(app: App, listener: Listener) { enum Gate {} impl Gate { fn mount<T>(app:T)->T {app} } let app=Gate::mount(app); axum::serve(listener,app); }",
            false,
        ),
        (
            "union",
            "use boss_core::machine_gate as Gate; fn main(app: App, listener: Listener) { union Gate { value:u8 } impl Gate { fn mount<T>(app:T)->T {app} } let app=Gate::mount(app); axum::serve(listener,app); }",
            false,
        ),
        (
            "type-alias",
            "use boss_core::machine_gate as Gate; fn main(app: App, listener: Listener) { struct Identity; impl Identity { fn mount<T>(app:T)->T {app} } type Gate=Identity; let app=Gate::mount(app); axum::serve(listener,app); }",
            false,
        ),
        (
            "trait",
            "use boss_core::machine_gate as Gate; fn main(app: App, listener: Listener) { trait Gate {fn mount(app:Self)->Self;} impl Gate for App {fn mount(app:Self)->Self {app}} let app=Gate::mount(app); axum::serve(listener,app); }",
            false,
        ),
        (
            "function-generic",
            "use boss_core::machine_gate as Gate; fn main<Gate: IdentityMount>(app: App, listener: Listener) { let app=Gate::mount(app); axum::serve(listener,app); }",
            false,
        ),
        (
            "impl-generic",
            "use boss_core::machine_gate as Gate; impl<Gate: IdentityMount> Wrapper<Gate> { fn run(app: App, listener: Listener) {let app=Gate::mount(app); axum::serve(listener,app);} }",
            false,
        ),
        (
            "nested-type",
            "use boss_core::machine_gate as Gate; fn main() {struct Gate; impl Gate {fn mount<T>(app:T)->T {app}} fn run(app: App, listener: Listener) {let app=Gate::mount(app); axum::serve(listener,app);} }",
            false,
        ),
        (
            "enum-alias",
            "mod machine_gate {pub enum Mount {} impl Mount {pub fn mount<T>(app:T)->T{app}}} use machine_gate::Mount as Gate; fn main(app:App,listener:Listener){let app=Gate::mount(app); axum::serve(listener,app);}",
            false,
        ),
        (
            "restored-import",
            "use boss_core::machine_gate as Gate; fn main(app: App, listener: Listener) { {struct Gate; impl Gate {fn mount<T>(app:T)->T{app}}} let app=Gate::mount(app); axum::serve(listener,app); }",
            true,
        ),
        (
            "inner-type-import",
            "use boss_core::machine_gate as Gate; fn main(app: App, listener: Listener) {mod Gate {pub fn mount<T>(app:T)->T{app}} {use boss_core::machine_gate as Gate; let app=Gate::mount(app); axum::serve(listener,app);} }",
            true,
        ),
        (
            "inner-value-import",
            "use boss_core::machine_gate::mount as gate; fn main(app: App, listener: Listener) {fn gate<T>(app:T)->T{app} {use boss_core::machine_gate::mount as gate; let app=gate(app); axum::serve(listener,app);} }",
            true,
        ),
        (
            "separate-namespace",
            "use boss_core::machine_gate::mount as Gate; fn main(app: App,listener:Listener){mod Gate {} let app=Gate(app); axum::serve(listener,app);}",
            true,
        ),
    ] {
        let rows = server_gate_scan::scan(source).unwrap();
        assert_eq!(rows.len(), 1);
        if rows[0].gated != expected {
            mismatches.push(format!(
                "{name}: got {}, expected {expected}: {source}",
                rows[0].gated
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn qualified_self_is_not_an_imported_value_function() {
    fn imported_mount(app: u8) -> u8 {
        app + 1
    }
    struct Identity;
    impl Identity {
        fn mount(app: u8) -> u8 {
            app
        }
    }
    assert_eq!((imported_mount(7), <Identity>::mount(7)), (8, 7));
    let source = "use boss_core::machine_gate::mount; fn main(app:App,listener:Listener) {struct Identity; impl Identity {fn mount<T>(app:T)->T{app}} let app=<Identity>::mount(app); axum::serve(listener,app);}";
    assert!(!server_gate_scan::scan(source).unwrap()[0].gated);
}

#[test]
fn an_import_keeps_its_origin_and_a_group_self_import_rebinds_its_name() {
    for source in [
        "use boss_core::machine_gate::mount as gate; fn main(app:App,listener:Listener){mod boss_core {} let app=gate(app); axum::serve(listener,app);}",
        "use boss_core::machine_gate as Gate; fn main(app:App,listener:Listener){mod boss_core {} let app=Gate::mount(app); axum::serve(listener,app);}",
        "use boss_core::machine_gate; fn main(app:App,listener:Listener){mod machine_gate {} {use boss_core::machine_gate::{self}; let app=machine_gate::mount(app); axum::serve(listener,app);}}",
        "fn main(app:App,listener:Listener){let app=gate(app); axum::serve(listener,app);} use Gate::mount as gate; use boss_core::machine_gate as Gate;",
    ] {
        assert!(
            server_gate_scan::scan(source).unwrap()[0].gated,
            "lost import origin: {source}"
        );
    }
}
