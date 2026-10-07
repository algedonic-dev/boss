//! Assemble report wiring from explicit service-owned dependencies.
//! Refresh and shutdown remain the owning process's responsibility.
use crate::PolicyClient;
use crate::role_guard::RoleGuardReporter;
use crate::role_reader::SnapshotRoleReader;
use crate::role_reporting::{ReportModeSource, ReportTally, ReportingPolicyClient};
use axum::Router;
use std::sync::Arc;

/// Shared bounds for production report owners; callers still own dependencies.
pub const SNAPSHOT_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);
pub const REFRESH_CADENCE: std::time::Duration = std::time::Duration::from_secs(10);
pub const REPORT_CAPACITY: usize = 512;

pub struct RoleReportWiring {
    pub policy: Arc<dyn PolicyClient>,
    pub guards: Arc<RoleGuardReporter>,
    pub inventory: Router,
}

pub fn assemble(
    service: &'static str,
    path: &'static str,
    original: Arc<dyn PolicyClient>,
    roles: Arc<SnapshotRoleReader>,
    mode: Arc<dyn ReportModeSource>,
    tally: Arc<ReportTally>,
) -> RoleReportWiring {
    let inventory = crate::role_inventory::router(
        service,
        path,
        original.clone(),
        roles.clone(),
        mode.clone(),
        tally.clone(),
    );
    let guards = Arc::new(RoleGuardReporter::new(
        roles.clone(),
        tally.clone(),
        mode.clone(),
    ));
    let policy = Arc::new(ReportingPolicyClient::with_mode_source(
        original, roles, tally, mode,
    ));
    RoleReportWiring {
        policy,
        guards,
        inventory,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RoleServiceError {
    #[error("serving role-report service: {0}")]
    Serve(#[from] std::io::Error),
    #[error("joining role snapshot owner: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("role snapshot owner: {0}")]
    Refresh(#[from] boss_core::role_of_record::RoleLookupError),
}

/// Serve under one explicit lifetime: shutdown stops and joins refresh,
/// including a source that never answers, before returning to the owner.
pub async fn serve_with_refresh(
    listener: tokio::net::TcpListener,
    app: axum::extract::connect_info::IntoMakeServiceWithConnectInfo<Router, std::net::SocketAddr>,
    roles: Arc<SnapshotRoleReader>,
    source: Arc<dyn crate::role_reader::RoleSnapshotSource>,
    mode: Arc<dyn ReportModeSource>,
    cadence: std::time::Duration,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), RoleServiceError> {
    if cadence.is_zero() {
        return Err(boss_core::role_of_record::RoleLookupError::Unavailable(
            "actor-role refresh cadence must be nonzero".into(),
        )
        .into());
    }
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let refresh = tokio::spawn(roles.run_refresh_loop(source, mode, cadence, stopped));
    let shutdown_stop = stop.clone();
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            // Stop refresh at the signal, before long-lived HTTP streams drain.
            let _ = shutdown_stop.send(true);
        })
        .await;
    let _ = stop.send(true);
    refresh.await??;
    served?;
    Ok(())
}

/// The existing service shutdown signals, shared so every refresh owner
/// gets the same process lifetime on both interactive and managed hosts.
pub async fn shutdown_signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "interrupt signal unavailable");
            std::future::pending::<()>().await;
        }
    };
    // Gate evidence owns process termination; this path owns interrupts.
    interrupt.await;
}
