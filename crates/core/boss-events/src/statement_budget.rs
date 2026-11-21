//! A monotonic SQL preparation budget, not a COMMIT completion deadline.
use sqlx::PgConnection;

#[derive(Clone, Copy)]
pub struct StatementBudget {
    deadline: tokio::time::Instant,
    statement_limit: Option<std::time::Duration>,
}

impl StatementBudget {
    /// Add a caller-owned cooperative refusal check without changing the
    /// deadline or installing a background cancellation task.
    pub fn checked<'a>(
        &self,
        check: &'a (dyn Fn() -> Result<(), String> + Sync),
    ) -> StatementControl<'a> {
        StatementControl {
            budget: *self,
            check,
        }
    }
    pub fn until(deadline: tokio::time::Instant) -> Self {
        Self {
            deadline,
            statement_limit: None,
        }
    }
    /// Keep an adapter's existing per-statement ceiling while spending
    /// the original absolute flight budget across all its statements.
    pub fn with_statement_limit(mut self, limit: std::time::Duration) -> Self {
        self.statement_limit = Some(limit);
        self
    }
    pub async fn refresh(&self, connection: &mut PgConnection) -> Result<(), String> {
        let remaining = self
            .deadline
            .checked_duration_since(tokio::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| "pre-commit statement budget expired".to_string())?;
        let remaining = self
            .statement_limit
            .map_or(remaining, |limit| remaining.min(limit));
        let setting = format!("{}ms", remaining.as_millis().max(1));
        tokio::time::timeout_at(self.deadline, async {
            sqlx::query("SELECT set_config('statement_timeout', $1, true), set_config('lock_timeout', $1, true)")
                .bind(setting).execute(connection).await.map_err(|error| error.to_string())
        }).await.map_err(|_| "pre-commit statement budget expired".to_string())??;
        if tokio::time::Instant::now() >= self.deadline {
            return Err("pre-commit statement budget expired".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct StatementControl<'a> {
    budget: StatementBudget,
    check: &'a (dyn Fn() -> Result<(), String> + Sync),
}

impl StatementControl<'_> {
    pub async fn refresh(&self, connection: &mut PgConnection) -> Result<(), String> {
        (self.check)()?;
        self.budget.refresh(connection).await?;
        (self.check)()
    }
}

pub async fn refresh_control(
    connection: &mut PgConnection,
    control: Option<StatementControl<'_>>,
) -> Result<(), String> {
    match control {
        Some(control) => control.refresh(connection).await,
        None => Ok(()),
    }
}

/// The budget governs the statements before COMMIT, never COMMIT itself.
/// COMMIT runs deferred triggers under whatever the transaction last SET
/// LOCAL, and while statement_timeout does not cancel it, lock_timeout does:
/// a deferred lock wait past the remaining budget aborted the selection
/// (measured by review run a0fd6902, reproduced in boss-jobs'
/// commit_lock_wait_is_not_canceled_by_the_pre_dispatch_lock_budget;
/// backlog 0325ca3e). So the last statement before COMMIT hands both
/// settings back to the session's own defaults.
pub async fn release_before_commit(connection: &mut PgConnection) -> Result<(), String> {
    for reset in [
        "SET LOCAL statement_timeout TO DEFAULT",
        "SET LOCAL lock_timeout TO DEFAULT",
    ] {
        sqlx::query(reset)
            .execute(&mut *connection)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Legacy adapter operations have no selection deadline; joined operations
/// supply the same absolute budget before every statement they dispatch.
pub async fn refresh(
    connection: &mut PgConnection,
    budget: Option<StatementBudget>,
) -> Result<(), String> {
    match budget {
        Some(budget) => budget.refresh(connection).await,
        None => Ok(()),
    }
}

// The fold fixtures' stall and budget, spelled once for every crate whose
// tests hand a budget to a Postgres read: boss-jobs' adapters and their
// tests, and this crate's audit-tail tests. Until backlog 03a5dbdc the
// 5254 ms was spelled four times and the 300 s twice, because boss-jobs
// kept them `pub(crate)` and nothing downstream of it could import them.
// Behind `test-support`, which no production build enables.

/// The BEGIN a gate's drive measured when the cuts envelope first failed a
/// train (gate-run a5c6c1ea, `begin 5254 ms`): a stalled drive, not a
/// plan, and longer than the whole production budget. A reader whose
/// clock reports it has already spent this much states the stall instead
/// of waiting for one.
#[cfg(any(test, feature = "test-support"))]
pub const MEASURED_STALL: std::time::Duration = std::time::Duration::from_millis(5254);

/// A fold fixture's budget: six times the worst BEGIN a gate drive has
/// recorded under the cuts envelope (47686 ms, gate-run f5c58280). It
/// bounds a hung fixture; it claims nothing about production.
#[cfg(any(test, feature = "test-support"))]
pub const FIXTURE_BUDGET: std::time::Duration = std::time::Duration::from_secs(300);

/// The clock a fold fixture measures from: [`MEASURED_STALL`] already lost.
#[cfg(any(test, feature = "test-support"))]
pub fn after_the_measured_stall() -> tokio::time::Instant {
    tokio::time::Instant::now() - MEASURED_STALL
}
