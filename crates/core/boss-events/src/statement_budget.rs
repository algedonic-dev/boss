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
