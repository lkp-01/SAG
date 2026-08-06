use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("blocking task: {0}")]
    Task(String),
    #[error("postgres: {0}")]
    Postgres(tokio_postgres::Error),
    #[error("postgres pool: {0}")]
    PostgresPool(String),
    #[error("postgres pool acquisition timed out after {timeout_ms}ms")]
    PostgresPoolAcquireTimeout { timeout_ms: u64 },
    #[error("storage configuration: {0}")]
    Configuration(String),
    #[error("storage validation: {0}")]
    Validation(String),
    #[error("storage conflict: {0}")]
    Conflict(String),
    #[error("storage invariant: {0}")]
    Invariant(String),
}

impl From<tokio_postgres::Error> for StorageError {
    fn from(error: tokio_postgres::Error) -> Self {
        if error
            .code()
            .is_some_and(|code| code == &tokio_postgres::error::SqlState::QUERY_CANCELED)
        {
            metrics::counter!("db_query_timeout_total").increment(1);
        }
        Self::Postgres(error)
    }
}
