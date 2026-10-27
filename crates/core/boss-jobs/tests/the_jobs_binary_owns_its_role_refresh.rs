//! tree-wide pin — the jobs binary delegates both signals to the shared
//! process owner, then cancels and joins actual work before releasing it.

const BINARY: &str = include_str!("../src/bin/boss_jobs_api.rs");
const CORE: &str = include_str!("../../boss-core/src/gate_evidence.rs");

fn compact(source: &str) -> String {
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

fn follows(source: &str, fragments: &[&str]) -> bool {
    let mut remaining = source;
    for fragment in fragments {
        let Some(at) = remaining.find(fragment) else {
            return false;
        };
        remaining = &remaining[at + fragment.len()..];
    }
    true
}

fn contract_errors(binary: &str, core: &str) -> Vec<&'static str> {
    let binary = compact(binary);
    let core = compact(core);
    let mut errors = Vec::new();
    for required in [
        "ReportingPolicyClient::with_mode_source(",
        "boss_policy_client::role_inventory::router(",
        ".run_refresh_loop(",
        "boss_core::machine_gate::mount(",
    ] {
        if !binary.contains(required) {
            errors.push(required);
        }
    }
    if !follows(
        &binary,
        &[
            "letprocess_shutdown=boss_core::gate_evidence::ShutdownGuard::hold();",
            "boss_core::machine_gate::mount(",
            "process_shutdown.requested().await;",
            "cancel_tx.send(true)",
            "membership_observation_owner.shutdown().await;",
            "role_refresh.await",
            "http_task.await",
            "projection_task.await",
            "process_shutdown.finish().await;",
        ],
    ) {
        errors.push("serving lifetime must hold, request, cancel, join, then finish");
    }
    if binary.contains("SignalKind::terminate()") {
        errors.push("gate evidence owns termination");
    }
    // Follow implementation, not merely a helper's name: an empty helper
    // would let the source pin pass while the actual service exits unjoined.
    let guard = core
        .split_once("implShutdownGuard{")
        .and_then(|(_, rest)| rest.split_once("implOpen{"))
        .map(|(body, _)| body)
        .unwrap_or("");
    if !guard.contains("_open:Open::hold()") {
        errors.push("guard must retain actual Open");
    }
    if !follows(
        guard,
        &[
            "pubasyncfnrequested(&self)",
            "tokio::signal::ctrl_c().await",
            "()=terminated()=>{}",
        ],
    ) {
        errors.push("guard must await interrupt and existing termination notification");
    }
    if !follows(
        guard,
        &[
            "pubasyncfnfinish(self)",
            "letterminating=*term().borrow();",
            "drop(self);",
            "ifterminating{std::future::pending::<()>().await;",
        ],
    ) {
        errors.push("finish must retain runtime for actual evidence drain");
    }
    if !follows(
        &core,
        &[
            "implOpen{",
            "watch_for_sigterm();",
            "OPEN.fetch_add(1,Ordering::SeqCst)",
        ],
    ) {
        errors.push("Open must register the existing watcher and retain ownership");
    }
    if !follows(
        &core,
        &[
            "fnwatch_for_sigterm()",
            "signal(SignalKind::terminate())",
            "sigterm.recv().await;",
            "term().send_replace(true);",
            "tokio::time::sleep(END_GRACE).await;",
        ],
    ) {
        errors.push("existing watcher must publish termination and retain its grace");
    }
    if !follows(
        &core,
        &[
            "asyncfnterminated()",
            "term().subscribe()",
            "told.wait_for(|t|*t).await",
        ],
    ) {
        errors.push("termination notification must read the existing watch");
    }
    errors
}

#[test]
fn jobs_mounts_local_snapshot_comparison_and_joins_refresh_on_shutdown() {
    assert_eq!(contract_errors(BINARY, CORE), Vec::<&str>::new());
}

#[test]
fn the_ownership_pin_rejects_missing_handlers_joins_and_empty_helpers() {
    for removed in [
        "boss_core::gate_evidence::ShutdownGuard::hold()",
        "process_shutdown.requested().await;",
        "cancel_tx.send(true)",
        "membership_observation_owner.shutdown().await;",
        "role_refresh",
        "http_task.await",
        "projection_task",
        "process_shutdown.finish().await;",
        "boss_policy_client::role_inventory::router(",
        "boss_core::machine_gate::mount(",
    ] {
        assert!(
            BINARY.contains(removed),
            "control must alter actual source: {removed}"
        );
        assert!(
            !contract_errors(&BINARY.replace(removed, "REMOVED"), CORE).is_empty(),
            "missed {removed}"
        );
    }
    for removed in [
        "Open::hold()",
        "tokio::signal::ctrl_c().await",
        "() = terminated() => {},",
        "drop(self);",
        "std::future::pending::<()>().await;",
        "watch_for_sigterm();",
        "OPEN.fetch_add(1, Ordering::SeqCst)",
        "term().send_replace(true);",
        "tokio::time::sleep(END_GRACE).await;",
        "term().subscribe()",
    ] {
        assert!(
            CORE.contains(removed),
            "control must alter actual source: {removed}"
        );
        assert!(
            !contract_errors(BINARY, &CORE.replace(removed, "REMOVED")).is_empty(),
            "missed {removed}"
        );
    }
    let second_listener = format!("{BINARY}\nSignalKind::terminate()");
    assert!(!contract_errors(&second_listener, CORE).is_empty());
    let premature = BINARY
        .replace(
            "process_shutdown.requested().await;",
            "process_shutdown.finish().await; process_shutdown.requested().await;",
        )
        .replacen(
            "    process_shutdown.finish().await;\n    Ok(())",
            "    Ok(())",
            1,
        );
    assert!(!contract_errors(&premature, CORE).is_empty());
}
