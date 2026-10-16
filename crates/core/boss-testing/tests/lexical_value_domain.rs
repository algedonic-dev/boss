mod server_gate_scan;

#[test]
fn opaque_macro_references_cannot_preserve_value_origins() {
    async fn argument(listener: tokio::net::TcpListener) {
        macro_rules! replace {
            ($value:expr) => {
                $value = axum::Router::new()
                    .into_make_service_with_connect_info::<std::net::SocketAddr>();
            };
        }
        let mut app = boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None);
        let _before = app.clone();
        replace!(app);
        let _ = axum::serve(listener, app.clone());
    }
    async fn captured(listener: tokio::net::TcpListener) {
        let mut app = boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None);
        let _before = app.clone();
        macro_rules! replace {
            () => {
                app = axum::Router::new()
                    .into_make_service_with_connect_info::<std::net::SocketAddr>();
            };
        }
        replace!();
        let _ = axum::serve(listener, app.clone());
    }
    async fn logging(listener: tokio::net::TcpListener) {
        let app = boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None);
        println!("a literal mentions app without passing its value");
        let other = 7;
        println!("{other}");
        let _ = axum::serve(listener, app.clone());
    }
    let _ = (argument, captured, logging);
    for (body, expected) in [
        ("replace!(app);axum::serve(listener,app.clone());", false),
        ("replace!((app));axum::serve(listener,app.clone());", false),
        (
            "macro_rules! replace {()=>{app=ungated;}} replace!();axum::serve(listener,app.clone());",
            false,
        ),
        (
            "println!(\"app is a literal\");axum::serve(listener,app.clone());",
            true,
        ),
        (
            "println!(\"{}\",other);axum::serve(listener,app.clone());",
            true,
        ),
    ] {
        let source = format!(
            "fn main(app:App,listener:Listener){{let mut app=machine_gate::mount(app);{body}}}"
        );
        let rows = server_gate_scan::scan(&source).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gated, expected, "{source}: {rows:?}");
    }
}

#[test]
fn serving_arguments_are_visited_before_mount_proof() {
    async fn actual_types(listener: tokio::net::TcpListener) {
        type Service = axum::extract::connect_info::IntoMakeServiceWithConnectInfo<
            axum::Router,
            std::net::SocketAddr,
        >;
        fn replace(
            app: &mut Service,
            listener: tokio::net::TcpListener,
        ) -> tokio::net::TcpListener {
            *app =
                axum::Router::new().into_make_service_with_connect_info::<std::net::SocketAddr>();
            listener
        }
        let mut app = boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None);
        let _ = axum::serve(replace(&mut app, listener), app.clone());
    }
    let _ = actual_types;
    for (body, expected) in [
        (
            "axum::serve(replace(&mut app,listener),app.clone());",
            false,
        ),
        ("axum::serve(read(&app,listener),app.clone());", true),
        ("axum::serve(listener,app.clone());", true),
    ] {
        let source = format!(
            "fn main(app:App,listener:Listener){{let mut app=machine_gate::mount(app);{body}}}"
        );
        let rows = server_gate_scan::scan(&source).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gated, expected, "{source}: {rows:?}");
    }
}

#[test]
fn unknown_receiver_methods_cannot_preserve_a_mount() {
    async fn actual_types(listener: tokio::net::TcpListener) {
        let mut app = boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None);
        app.clone_from(
            &axum::Router::new().into_make_service_with_connect_info::<std::net::SocketAddr>(),
        );
        let _ = axum::serve(listener, app.clone());
    }
    let _ = actual_types;
    let mut native = 8u8;
    native.clone_from(&7);
    assert_eq!(native, 7);
    for (body, expected) in [
        (
            "app.clone_from(&ungated);axum::serve(listener,app.clone());",
            false,
        ),
        (
            "let mut alias=app;alias.clone_from(&ungated);axum::serve(listener,alias.clone());",
            false,
        ),
        ("app=ungated;axum::serve(listener,app.clone());", false),
        ("let alias=app.clone();axum::serve(listener,alias);", true),
        ("axum::serve(listener,app.into_make_service());", true),
        (
            "axum::serve(listener,app.into_make_service_with_connect_info::<SocketAddr>());",
            true,
        ),
    ] {
        let source = format!(
            "fn main(app:App,listener:Listener){{let mut app=machine_gate::mount(app);{body}}}"
        );
        let rows = server_gate_scan::scan(&source).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gated, expected, "{source}: {rows:?}");
    }
}

#[test]
fn mutable_reference_patterns_cannot_vouch_a_replaced_service() {
    #[expect(
        clippy::toplevel_ref_arg,
        reason = "compile the exact mutable binding pattern being refused"
    )]
    async fn direct(listener: tokio::net::TcpListener) {
        let ref mut app = boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None);
        *app = axum::Router::new().into_make_service_with_connect_info::<std::net::SocketAddr>();
        let _ = axum::serve(listener, app.clone());
    }
    async fn nested(listener: tokio::net::TcpListener) {
        let (ref mut app,) = (boss_core::machine_gate::mount(
            axum::Router::new(),
            "jobs",
            &[],
            None,
        ),);
        *app = axum::Router::new().into_make_service_with_connect_info::<std::net::SocketAddr>();
        let _ = axum::serve(listener, app.clone());
    }
    #[expect(
        clippy::match_single_binding,
        reason = "compile the exact mutable match pattern being refused"
    )]
    async fn matched(listener: tokio::net::TcpListener) {
        match boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None) {
            ref mut app => {
                *app = axum::Router::new()
                    .into_make_service_with_connect_info::<std::net::SocketAddr>();
                let _ = axum::serve(listener, app.clone());
            }
        }
    }
    #[expect(
        clippy::toplevel_ref_arg,
        reason = "compile the immutable reference-pattern positive"
    )]
    async fn immutable(listener: tokio::net::TcpListener) {
        let ref app = boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None);
        let _ = axum::serve(listener, app.clone());
    }
    let _ = (direct, nested, matched, immutable);
    for (body, expected) in [
        (
            "let ref mut app=machine_gate::mount(app);*app=ungated;axum::serve(listener,app.clone());",
            false,
        ),
        (
            "let (ref mut app,)=(machine_gate::mount(app),);*app=ungated;axum::serve(listener,app.clone());",
            false,
        ),
        (
            "match machine_gate::mount(app){ref mut app=>{*app=ungated;axum::serve(listener,app.clone());}}",
            false,
        ),
        (
            "let ref app=machine_gate::mount(app);axum::serve(listener,app.clone());",
            true,
        ),
        (
            "let app=machine_gate::mount(app);axum::serve(listener,app.clone());",
            true,
        ),
    ] {
        let rows = server_gate_scan::scan(&format!("fn main(app:App,listener:Listener){{{body}}}"))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gated, expected, "{body}: {rows:?}");
    }
}

#[test]
fn mutable_alias_cannot_keep_a_mount_after_replacement() {
    // This exact sequence also type-checks with the real mount and Axum
    // transport; compiling it opens no listener or gate-control files.
    async fn actual_types(listener: tokio::net::TcpListener) {
        let mut app = boss_core::machine_gate::mount(axum::Router::new(), "jobs", &[], None);
        let app = &mut app;
        *app = axum::Router::new().into_make_service_with_connect_info::<std::net::SocketAddr>();
        let _ = axum::serve(listener, app.clone());
    }
    let _ = actual_types;
    let mut native = 8u8;
    let native = &mut native;
    *native = 7;
    assert_eq!(*native, 7);
    let source = "async fn actual_types(listener:tokio::net::TcpListener){let mut app=boss_core::machine_gate::mount(axum::Router::new(),\"jobs\",&[],None);let app=&mut app;*app=axum::Router::new().into_make_service_with_connect_info::<std::net::SocketAddr>();let _=axum::serve(listener,app.clone());}";
    let rows = server_gate_scan::scan(source).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0].gated,
        "replaced mutable alias retains a mount: {rows:?}"
    );
}

#[test]
fn initializer_mutations_do_not_transfer_stale_value_origins() {
    for (body, expected) in [
        ("axum::serve(listener,app.clone());", true),
        ("let alias=app;axum::serve(listener,alias);", true),
        ("let alias=&app;axum::serve(listener,alias.clone());", true),
        ("app=ungated;axum::serve(listener,app.clone());", false),
        (
            "let app=&mut app;*app=ungated;axum::serve(listener,app.clone());",
            false,
        ),
        (
            "let alias=&mut app;*alias=ungated;axum::serve(listener,alias.clone());",
            false,
        ),
        (
            "let alias=&mut app;axum::serve(listener,app.clone());",
            false,
        ),
    ] {
        let source = format!(
            "fn main(app:App,listener:Listener){{let mut app=machine_gate::mount(app);{body}}}"
        );
        let rows = server_gate_scan::scan(&source).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gated, expected, "{source}: {rows:?}");
    }
}

#[test]
#[expect(
    non_snake_case,
    reason = "native locals deliberately share associated-member spelling"
)]
fn associated_members_are_not_the_outer_lexical_values() {
    struct Factory;
    impl Factory {
        const LISTENER: u8 = 7;
        const APP: u8 = 7;
    }
    fn serve<L>(listener: L, app: u8) -> (L, u8) {
        (listener, app)
    }
    let LISTENER = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let APP = 8;
    let address = LISTENER.local_addr().unwrap();
    assert_eq!(serve(<Factory>::LISTENER, APP), (7, 8));
    assert_eq!(serve(7, <Factory>::APP), (7, 7));
    assert_eq!(LISTENER.local_addr().unwrap(), address);
    let (original, app) = serve(LISTENER, APP);
    assert_eq!(original.local_addr().unwrap(), address);
    assert_eq!(app, 8);
    let positives=server_gate_scan::scan("fn main(app:App){let LISTENER=std::net::TcpListener::bind(addr).unwrap(); let APP=machine_gate::mount(app); axum::serve(LISTENER,APP);}").unwrap();
    assert_eq!(positives.len(), 1);
    assert!(positives[0].gated);
    let listener=server_gate_scan::scan("fn main(app:App){let LISTENER=std::net::TcpListener::bind(addr).unwrap(); let APP=machine_gate::mount(app); axum::serve(<Factory>::LISTENER,APP);}").unwrap();
    let app=server_gate_scan::scan("fn main(listener:Listener,app:App){let APP=machine_gate::mount(app); axum::serve(listener,<Factory>::APP);}").unwrap();
    assert_eq!(listener[0].function, "main");
    assert_eq!(listener[0].kind, "HTTP serve");
    let missing_listener = !listener
        .iter()
        .any(|row| row.kind == "unaccounted TCP listener");
    let borrowed_app = app[0].gated;
    assert!(
        !missing_listener && !borrowed_app,
        "qualified members borrow lexical values: listener={listener:?}; app={app:?}"
    );
}

#[test]
fn only_plain_value_paths_carry_listener_and_app_origins() {
    let mut failures = Vec::new();
    for expression in [
        "VALUE",
        "(VALUE)",
        "<Factory>::VALUE",
        "::VALUE",
        "VALUE::<u8>",
    ] {
        let plain = matches!(expression, "VALUE" | "(VALUE)");
        let listener = server_gate_scan::scan(&format!(
            "fn main(app:App){{let VALUE=std::net::TcpListener::bind(addr).unwrap();let mounted=machine_gate::mount(app);axum::serve({expression},mounted);}}"
        )).unwrap();
        let app = server_gate_scan::scan(&format!(
            "fn main(listener:Listener,app:App){{let VALUE=machine_gate::mount(app);axum::serve(listener,{expression});}}"
        )).unwrap();
        let alias_listener = server_gate_scan::scan(&format!(
            "fn main(app:App){{let VALUE=std::net::TcpListener::bind(addr).unwrap();let mounted=machine_gate::mount(app);let moved={expression};axum::serve(moved,mounted);}}"
        )).unwrap();
        let alias_app = server_gate_scan::scan(&format!(
            "fn main(listener:Listener,app:App){{let VALUE=machine_gate::mount(app);let moved={expression};axum::serve(listener,moved);}}"
        )).unwrap();
        for (label, rows, wanted) in [
            ("listener", &listener, plain),
            ("listener alias", &alias_listener, plain),
        ] {
            let accounted = !rows
                .iter()
                .any(|row| row.kind == "unaccounted TCP listener");
            if accounted != wanted {
                failures.push(format!("{label} {expression}: {rows:?}"));
            }
        }
        for (label, rows) in [("app", &app), ("app alias", &alias_app)] {
            if rows[0].gated != plain {
                failures.push(format!("{label} {expression}: {rows:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "unproved value paths borrow lexical origins: {failures:#?}"
    );
}
