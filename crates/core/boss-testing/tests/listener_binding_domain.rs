mod server_gate_scan;

#[test]
fn compiled_transport_shadow_and_positive() {
    mod actual {
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
    use actual as server;
    assert_eq!(server::bind(0).serve(8), 8);
    {
        mod server {
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
        assert_eq!(server::bind(0).serve(8), 7);
    }
    assert_eq!(server::bind(0).serve(8), 8);
    for (body, expected) in [
        ("server::bind(0).serve(app)", true),
        ("{mod server {} server::bind(0).serve(app)}", false),
    ] {
        let rows=server_gate_scan::scan(&format!("use axum_server as server; fn main(app:App){{let app=machine_gate::mount(app);{body};}}" )).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gated, expected);
    }
}

#[test]
fn consuming_a_shadow_listener_does_not_account_for_the_original_socket() {
    mod axum {
        pub fn serve(listener: u8, app: u8) -> (u8, u8) {
            (listener, app)
        }
    }
    mod machine_gate {
        pub fn mount(app: u8) -> u8 {
            app + 1
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let original_address = listener.local_addr().unwrap();
    {
        let listener = 7u8;
        let app = machine_gate::mount(7);
        assert_eq!(axum::serve(listener, app), (7, 8));
    }
    assert_eq!(listener.local_addr().unwrap(), original_address);
    // Native lexical witness above keeps the first socket alive while the
    // nested name denotes a different value. The actual scanner must retain it.
    let source = "fn main(other:Listener,app:App){let listener=std::net::TcpListener::bind(addr).unwrap(); {let listener=other; let app=machine_gate::mount(app); axum::serve(listener,app);}}";
    let rows = server_gate_scan::scan(source).unwrap();
    assert!(
        rows.iter()
            .any(|r| r.kind == "unaccounted TCP listener" && !r.gated),
        "original socket disappeared: {rows:?}"
    );
}

#[test]
fn actual_original_listener_consumption_is_accounted_once() {
    let rows=server_gate_scan::scan("fn main(app:App){let listener=std::net::TcpListener::bind(addr).unwrap(); let app=machine_gate::mount(app); axum::serve(listener,app);}").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "HTTP serve");
    assert_eq!(rows[0].function, "main");
    assert!(rows[0].gated);
}

#[test]
fn listener_names_never_substitute_for_lexical_socket_origins() {
    let native = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = native.local_addr().unwrap();
    let moved = native;
    assert_eq!(moved.local_addr().unwrap(), address);
    let prefix = "fn main(other:Listener,app:App){let mut listener=std::net::TcpListener::bind(addr).unwrap(); let app=machine_gate::mount(app);";
    let cases = [
        ("{let listener=other; axum::serve(listener,app);}", 1),
        ("let listener=other; axum::serve(listener,app);", 1),
        ("listener=other; axum::serve(listener,app);", 1),
        ("{let (listener,)=(other,); axum::serve(listener,app);}", 1),
        ("match other {listener=>{axum::serve(listener,app);}}", 1),
        ("let closure=|listener|axum::serve(listener,app);", 1),
        ("for listener in others {axum::serve(listener,app);}", 1),
        (
            "while let Some(listener)=other {axum::serve(listener,app);}",
            1,
        ),
        ("let moved=listener; axum::serve(moved,app);", 0),
        ("{let listener=other;} axum::serve(listener,app);", 0),
        (
            "{let listener=std::net::TcpListener::bind(addr2).unwrap(); axum::serve(listener,app.clone());} axum::serve(listener,app);",
            0,
        ),
    ];
    let mut failures = Vec::new();
    for (body, expected) in cases {
        let rows = server_gate_scan::scan(&format!("{prefix}{body}}}")).unwrap();
        let actual = rows
            .iter()
            .filter(|row| row.kind == "unaccounted TCP listener")
            .count();
        if actual != expected {
            failures.push((body, expected, rows));
        }
    }
    assert!(
        failures.is_empty(),
        "listener origin mismatches: {failures:?}"
    );
}
