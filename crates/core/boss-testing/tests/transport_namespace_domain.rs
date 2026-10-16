mod server_gate_scan;

#[test]
fn a_local_builder_namespace_cannot_prove_the_transport() {
    mod actual {
        pub mod server {
            pub fn bind(_: u8) -> Builder {
                Builder
            }
            pub struct Builder;
            impl Builder {
                pub fn serve(self, app: u8) -> u8 {
                    app
                }
            }
        }
    }
    use actual::server as axum_server;
    assert_eq!(axum_server::bind(0).serve(8), 8);
    let actual = {
        mod axum_server {
            pub fn bind(_: u8) -> Builder {
                Builder
            }
            pub struct Builder;
            impl Builder {
                pub fn serve(self, _: u8) -> u8 {
                    7
                }
            }
        }
        axum_server::bind(0).serve(8)
    };
    assert_eq!(actual, 7);
    assert_eq!(axum_server::bind(0).serve(8), 8);
    let source = "use axum_server as server; fn main(app:App) { mod server {pub fn bind(_:u8)->Builder {Builder} pub struct Builder; impl Builder {pub fn serve(self,_:App)->App{ungated()}}} let app=machine_gate::mount(app); server::bind(0).serve(app); }";
    let rows = server_gate_scan::scan(source).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].function, "main");
    assert_eq!(rows[0].kind, "HTTP builder serve");
    assert!(
        !rows[0].gated,
        "local builder borrows transport provenance: {rows:?}"
    );
}

#[test]
fn real_builder_positive_is_preserved() {
    let rows=server_gate_scan::scan("use axum_server as server; fn main(app:App){let app=machine_gate::mount(app); server::bind(addr).serve(app);}").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].gated);
}

#[test]
fn every_known_transport_uses_its_actual_lexical_namespace() {
    mod imported {
        pub fn serve(_: u8, app: u8) -> u8 {
            app
        }
    }
    use imported as axum;
    assert_eq!(axum::serve(0, 8), 8);
    {
        mod axum {
            pub fn serve(_: u8, _: u8) -> u8 {
                7
            }
        }
        assert_eq!(axum::serve(0, 8), 7);
    }
    assert_eq!(axum::serve(0, 8), 8);
    let cases = [
        (
            "use axum_server as server; fn main(app:App){let app=machine_gate::mount(app); server::bind(0).serve(app);}",
            true,
        ),
        (
            "use hyper::Server as Server; fn main(app:App){let app=machine_gate::mount(app); Server::bind(0).serve(app);}",
            true,
        ),
        (
            "use axum_server::bind; fn main(app:App){let app=machine_gate::mount(app); bind(0).serve(app);}",
            true,
        ),
        (
            "use axum::serve as run; fn main(app:App,l:Listener){let app=machine_gate::mount(app); run(l,app);}",
            true,
        ),
        (
            "use boss_policy_client::role_service::serve_with_refresh as run; fn main(app:App,l:Listener){let app=machine_gate::mount(app); run(l,app);}",
            true,
        ),
        (
            "use axum_server as server; fn main(app:App){let server=7; let app=machine_gate::mount(app); server::bind(0).serve(app);}",
            true,
        ),
        (
            "use axum_server as server; fn main(app:App){struct server; let app=machine_gate::mount(app); server::bind(0).serve(app);}",
            false,
        ),
        (
            "use hyper::Server as Server; fn main<Server>(app:App){let app=machine_gate::mount(app); Server::bind(0).serve(app);}",
            false,
        ),
        (
            "mod axum_server{} use axum_server as server; fn main(app:App){let app=machine_gate::mount(app); server::bind(0).serve(app);}",
            false,
        ),
        (
            "fn main(app:App){let app=machine_gate::mount(app); use fake::*; axum_server::bind(0).serve(app);}",
            false,
        ),
        (
            "fn main(app:App){let app=machine_gate::mount(app); {use fake::*; axum_server::bind(0).serve(app);}}",
            false,
        ),
        (
            "use axum_server::bind; fn main(app:App,bind:fn(u8)->Builder){let app=machine_gate::mount(app); bind(0).serve(app);}",
            false,
        ),
        (
            "use axum_server::bind; fn main(app:App){let app=machine_gate::mount(app); let (bind,)=(identity,); bind(0).serve(app);}",
            false,
        ),
        (
            "use axum_server::bind; fn main(app:App){let app=machine_gate::mount(app); let closure=|bind|bind(0).serve(app);}",
            false,
        ),
        (
            "fn main(app:App,l:Listener){mod axum{} let app=machine_gate::mount(app); axum::serve(l,app);}",
            false,
        ),
        (
            "mod axum{} use axum::serve as run; fn main(app:App,l:Listener){let app=machine_gate::mount(app); run(l,app);}",
            false,
        ),
        (
            "fn main(app:App,l:Listener){mod boss_policy_client{} let app=machine_gate::mount(app); boss_policy_client::role_service::serve_with_refresh(l,app);}",
            false,
        ),
        (
            "fn main(app:App,l:Listener){let app=machine_gate::mount(app); arbitrary::role_service::serve_with_refresh(l,app);}",
            false,
        ),
        (
            "fn main(app:App,l:Listener){let app=machine_gate::mount(app); use fake::*; axum::serve(l,app);}",
            false,
        ),
        (
            "fn main(app:App,l:Listener){let app=machine_gate::mount(app); {use fake::*; axum::serve(l,app);}}",
            false,
        ),
    ];
    let mut failures = Vec::new();
    for (source, expected) in cases {
        let rows = server_gate_scan::scan(source).unwrap();
        if rows.len() != 1 || rows[0].gated != expected {
            failures.push((source, expected, rows));
        }
    }
    assert!(
        failures.is_empty(),
        "transport provenance mismatches: {failures:?}"
    );
}

#[test]
fn transport_scope_restores_and_captured_import_origins_stay_valid() {
    let source = "use axum_server as server; fn main(app:App){let app=machine_gate::mount(app); {mod server{} server::bind(0).serve(app);} server::bind(0).serve(app);}";
    let rows = server_gate_scan::scan(source).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(!rows[0].gated);
    assert!(rows[1].gated);
    let source = "use axum_server as server; fn main(app:App){mod axum_server{} let app=machine_gate::mount(app); server::bind(0).serve(app);}";
    let rows = server_gate_scan::scan(source).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].gated);
}
