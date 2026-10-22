//! A monotonic SQL preparation budget, not a COMMIT completion deadline.
use sqlx::PgConnection;

#[derive(Clone, Copy)]
pub struct StatementBudget(tokio::time::Instant);

impl StatementBudget {
    pub fn until(deadline: tokio::time::Instant) -> Self {
        Self(deadline)
    }
    pub async fn refresh(&self, connection: &mut PgConnection) -> Result<(), String> {
        let remaining = self
            .0
            .checked_duration_since(tokio::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| "pre-commit statement budget expired".to_string())?;
        let setting = format!("{}ms", remaining.as_millis().max(1));
        tokio::time::timeout_at(self.0, async {
            sqlx::query("SELECT set_config('statement_timeout', $1, true), set_config('lock_timeout', $1, true)")
                .bind(setting).execute(connection).await.map_err(|error| error.to_string())
        }).await.map_err(|_| "pre-commit statement budget expired".to_string())??;
        if tokio::time::Instant::now() >= self.0 {
            return Err("pre-commit statement budget expired".into());
        }
        Ok(())
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
