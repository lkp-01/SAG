use crate::paths::{ensure_storage_dir_for_path, resolve_storage_db_path};
use crate::sqlite_store::SqliteStore;
use crate::StorageError;
use deadpool_postgres::{
    Hook, HookError, Manager, ManagerConfig, Pool, PoolError, RecyclingMethod, Runtime, TimeoutType,
};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_postgres::NoTls;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    Sqlite,
    Postgres,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostgresPoolConfig {
    pub max_size: usize,
    pub acquire_timeout: Duration,
    pub connect_timeout: Duration,
    pub query_timeout: Duration,
}

impl Default for PostgresPoolConfig {
    fn default() -> Self {
        Self {
            max_size: 16,
            acquire_timeout: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(5),
            query_timeout: Duration::from_secs(5),
        }
    }
}

impl PostgresPoolConfig {
    pub fn from_env() -> Result<Self, StorageError> {
        fn env_usize(name: &str, default: usize) -> Result<usize, StorageError> {
            std::env::var(name)
                .ok()
                .map(|value| {
                    value.parse::<usize>().map_err(|_| {
                        StorageError::Configuration(format!("{name} must be a positive integer"))
                    })
                })
                .transpose()
                .map(|value| value.unwrap_or(default))
        }

        fn env_duration_ms(name: &str, default_ms: u64) -> Result<Duration, StorageError> {
            std::env::var(name)
                .ok()
                .map(|value| {
                    value
                        .parse::<u64>()
                        .map(Duration::from_millis)
                        .map_err(|_| {
                            StorageError::Configuration(format!(
                                "{name} must be a positive integer number of milliseconds"
                            ))
                        })
                })
                .transpose()
                .map(|value| value.unwrap_or_else(|| Duration::from_millis(default_ms)))
        }

        let config = Self {
            max_size: env_usize("SAG_POSTGRES_POOL_MAX_SIZE", 16)?,
            acquire_timeout: env_duration_ms("SAG_POSTGRES_POOL_ACQUIRE_TIMEOUT_MS", 2_000)?,
            connect_timeout: env_duration_ms("SAG_POSTGRES_CONNECT_TIMEOUT_MS", 5_000)?,
            query_timeout: env_duration_ms("SAG_POSTGRES_QUERY_TIMEOUT_MS", 5_000)?,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), StorageError> {
        if self.max_size == 0 {
            return Err(StorageError::Configuration(
                "SAG_POSTGRES_POOL_MAX_SIZE must be greater than zero".into(),
            ));
        }
        for (name, value) in [
            ("SAG_POSTGRES_POOL_ACQUIRE_TIMEOUT_MS", self.acquire_timeout),
            ("SAG_POSTGRES_CONNECT_TIMEOUT_MS", self.connect_timeout),
            ("SAG_POSTGRES_QUERY_TIMEOUT_MS", self.query_timeout),
        ] {
            if value.is_zero() {
                return Err(StorageError::Configuration(format!(
                    "{name} must be greater than zero"
                )));
            }
        }
        Ok(())
    }
}

pub fn validate_postgres_connection_budget(
    config: &PostgresPoolConfig,
    replica_count: usize,
    reserved_connections: usize,
    postgres_max_connections: usize,
) -> Result<(), StorageError> {
    let application_connections = replica_count
        .checked_mul(config.max_size)
        .ok_or_else(|| StorageError::Configuration("PostgreSQL pool budget overflow".into()))?;
    let total = application_connections
        .checked_add(reserved_connections)
        .ok_or_else(|| StorageError::Configuration("PostgreSQL pool budget overflow".into()))?;
    if replica_count == 0 || postgres_max_connections == 0 || total > postgres_max_connections {
        return Err(StorageError::Configuration(format!(
            "PostgreSQL connection budget exceeded: replicas({replica_count}) * pool({}) + reserved({reserved_connections}) = {total}, max_connections={postgres_max_connections}",
            config.max_size
        )));
    }
    Ok(())
}

fn env_connection_budget(config: &PostgresPoolConfig) -> Result<(), StorageError> {
    fn value(name: &str, default: usize) -> Result<usize, StorageError> {
        std::env::var(name)
            .ok()
            .map(|raw| {
                raw.parse::<usize>().map_err(|_| {
                    StorageError::Configuration(format!("{name} must be a positive integer"))
                })
            })
            .transpose()
            .map(|parsed| parsed.unwrap_or(default))
    }
    validate_postgres_connection_budget(
        config,
        value("SAG_POSTGRES_REPLICA_BUDGET", 1)?,
        value("SAG_POSTGRES_RESERVED_CONNECTIONS", 10)?,
        value("SAG_POSTGRES_MAX_CONNECTIONS", 100)?,
    )
}

#[derive(Clone)]
pub struct PostgresStore {
    dsn: String,
    pool: Pool,
    config: PostgresPoolConfig,
}

impl fmt::Debug for PostgresStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresStore")
            .field("pool", &self.pool.status())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl PostgresStore {
    pub fn new(dsn: impl Into<String>) -> Self {
        let config = PostgresPoolConfig::from_env()
            .unwrap_or_else(|error| panic!("invalid PostgreSQL pool configuration: {error}"));
        env_connection_budget(&config)
            .unwrap_or_else(|error| panic!("invalid PostgreSQL connection budget: {error}"));
        Self::with_config(dsn, config)
            .unwrap_or_else(|error| panic!("failed to build PostgreSQL pool: {error}"))
    }

    pub fn with_config(
        dsn: impl Into<String>,
        config: PostgresPoolConfig,
    ) -> Result<Self, StorageError> {
        config.validate()?;
        let dsn = dsn.into();
        let mut postgres_config = dsn.parse::<tokio_postgres::Config>()?;
        postgres_config.connect_timeout(config.connect_timeout);
        if postgres_config.get_application_name().is_none() {
            let application_name =
                std::env::var("SAG_POSTGRES_APPLICATION_NAME").unwrap_or_else(|_| "sag".into());
            postgres_config.application_name(application_name);
        }

        let manager = Manager::from_config(
            postgres_config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Verified,
            },
        );
        let statement_timeout = Arc::new(format!(
            "SET statement_timeout = {}",
            config.query_timeout.as_millis()
        ));
        let pool = Pool::builder(manager)
            .max_size(config.max_size)
            .wait_timeout(Some(config.acquire_timeout))
            .create_timeout(Some(config.connect_timeout))
            .runtime(Runtime::Tokio1)
            .post_create(Hook::async_fn(move |client, _metrics| {
                let statement_timeout = Arc::clone(&statement_timeout);
                Box::pin(async move {
                    client
                        .batch_execute(&statement_timeout)
                        .await
                        .map_err(HookError::Backend)
                })
            }))
            .build()
            .map_err(|error| StorageError::PostgresPool(error.to_string()))?;

        Ok(Self { dsn, pool, config })
    }

    pub fn dsn(&self) -> &str {
        &self.dsn
    }

    pub fn pool_config(&self) -> PostgresPoolConfig {
        self.config
    }

    pub fn pool_status(&self) -> deadpool_postgres::Status {
        self.pool.status()
    }

    pub async fn client(&self) -> Result<deadpool_postgres::Client, StorageError> {
        let started = Instant::now();
        let result = self.pool.get().await;
        metrics::histogram!("db_pool_wait_seconds").record(started.elapsed().as_secs_f64());
        let status = self.pool.status();
        metrics::gauge!("db_pool_in_use").set(status.size.saturating_sub(status.available) as f64);
        metrics::gauge!("db_pool_available").set(status.available as f64);
        match result {
            Ok(client) => Ok(client),
            Err(PoolError::Timeout(TimeoutType::Wait)) => {
                metrics::counter!("db_pool_acquire_timeout_total").increment(1);
                Err(StorageError::PostgresPoolAcquireTimeout {
                    timeout_ms: self.config.acquire_timeout.as_millis() as u64,
                })
            }
            Err(error) => Err(StorageError::PostgresPool(error.to_string())),
        }
    }
}

#[derive(Clone)]
pub enum StorageStore {
    Sqlite(SqliteStore),
    Postgres(PostgresStore),
}

impl StorageStore {
    pub async fn health_check(&self) -> Result<(), StorageError> {
        match self {
            Self::Sqlite(store) => store.health_check().await,
            Self::Postgres(store) => {
                let client = store.client().await?;
                client.query_one("SELECT 1", &[]).await?;
                Ok(())
            }
        }
    }
}

pub fn resolve_storage_backend() -> StorageBackend {
    let v = std::env::var("SAG_STORAGE_BACKEND")
        .unwrap_or_else(|_| "sqlite".to_string())
        .to_lowercase();
    if v == "postgres" || v == "postgresql" || v == "pg" {
        StorageBackend::Postgres
    } else {
        StorageBackend::Sqlite
    }
}

pub fn resolve_postgres_dsn() -> String {
    // Dev-friendly default still points at local Postgres.
    // In real deployments (incl. dual-host over VPN/DNS), always set SAG_POSTGRES_DSN explicitly.
    std::env::var("SAG_POSTGRES_DSN").unwrap_or_else(|_| {
        if std::env::var("SAG_DOCKER_COMPOSE").ok().as_deref() == Some("1") {
            "postgres://postgres:postgres@postgres:5432/sag".to_string()
        } else {
            "postgres://postgres:postgres@127.0.0.1:5432/sag".to_string()
        }
    })
}

pub fn redact_postgres_dsn(dsn: &str) -> String {
    let Some(scheme_pos) = dsn.find("://") else {
        return dsn.to_string();
    };
    let after_scheme = &dsn[(scheme_pos + 3)..];
    let Some(at_pos_rel) = after_scheme.find('@') else {
        return dsn.to_string();
    };
    let creds = &after_scheme[..at_pos_rel];
    let rest = &after_scheme[(at_pos_rel + 1)..];
    if let Some(colon) = creds.find(':') {
        let user = &creds[..colon];
        format!("{}://{}:***@{}", &dsn[..scheme_pos], user, rest)
    } else {
        format!("{}://{}@{}", &dsn[..scheme_pos], creds, rest)
    }
}

pub fn build_store_from_env() -> StorageStore {
    match resolve_storage_backend() {
        StorageBackend::Sqlite => {
            let db_path = resolve_storage_db_path();
            ensure_storage_dir_for_path(&db_path);
            StorageStore::Sqlite(SqliteStore::new(db_path))
        }
        StorageBackend::Postgres => {
            let dsn = resolve_postgres_dsn();
            StorageStore::Postgres(PostgresStore::new(dsn))
        }
    }
}

pub async fn ensure_store_schema(store: &StorageStore) -> Result<(), StorageError> {
    match store {
        StorageStore::Sqlite(sqlite) => sqlite.ensure_schema().await,
        StorageStore::Postgres(pg) => {
            let client = pg.client().await?;
            client
                .batch_execute(
                    r#"
                    CREATE TABLE IF NOT EXISTS tunnel_routes (
                        host TEXT PRIMARY KEY,
                        app_id TEXT NOT NULL,
                        connector_endpoint TEXT NOT NULL,
                        require_healthy_tunnel BOOLEAN NOT NULL DEFAULT TRUE
                    );
                    CREATE TABLE IF NOT EXISTS intranet_upstreams (
                        app_id TEXT PRIMARY KEY,
                        upstream TEXT NOT NULL,
                        scheme TEXT NOT NULL DEFAULT 'http'
                    );
                    CREATE TABLE IF NOT EXISTS policies (
                        id TEXT PRIMARY KEY,
                        effect TEXT NOT NULL,
                        subjects_json TEXT NOT NULL,
                        app_id TEXT,
                        path_prefix TEXT,
                        priority INTEGER NOT NULL DEFAULT 1000
                    );
                    CREATE TABLE IF NOT EXISTS users (
                        id TEXT NOT NULL,
                        username TEXT PRIMARY KEY,
                        password_hash TEXT NOT NULL,
                        roles_json TEXT NOT NULL,
                        display_name TEXT,
                        title TEXT,
                        enabled BOOLEAN NOT NULL DEFAULT TRUE,
                        auth_version BIGINT NOT NULL DEFAULT 1,
                        updated_at_ms BIGINT NOT NULL DEFAULT 0
                    );
                    CREATE TABLE IF NOT EXISTS app_metrics_minute (
                        ts_minute BIGINT NOT NULL,
                        app_id TEXT NOT NULL,
                        request_count BIGINT NOT NULL DEFAULT 0,
                        pv_count BIGINT NOT NULL DEFAULT 0,
                        uv_count BIGINT NOT NULL DEFAULT 0,
                        unique_ip_count BIGINT NOT NULL DEFAULT 0,
                        err4xx_count BIGINT NOT NULL DEFAULT 0,
                        err5xx_count BIGINT NOT NULL DEFAULT 0,
                        qps_avg DOUBLE PRECISION NOT NULL DEFAULT 0,
                        PRIMARY KEY (ts_minute, app_id)
                    );
                    CREATE TABLE IF NOT EXISTS apps (
                        app_id TEXT PRIMARY KEY,
                        display_name TEXT NOT NULL,
                        description TEXT NOT NULL DEFAULT '',
                        enabled BOOLEAN NOT NULL DEFAULT TRUE
                    );
                    CREATE TABLE IF NOT EXISTS api_routes (
                        id TEXT PRIMARY KEY,
                        app_id TEXT NOT NULL,
                        method TEXT NOT NULL,
                        path TEXT NOT NULL,
                        enabled BOOLEAN NOT NULL DEFAULT TRUE,
                        description TEXT NOT NULL DEFAULT ''
                    );
                    CREATE TABLE IF NOT EXISTS identity_providers (
                        id TEXT PRIMARY KEY,
                        kind TEXT NOT NULL,
                        issuer TEXT NOT NULL DEFAULT '',
                        client_id TEXT NOT NULL DEFAULT '',
                        client_secret TEXT NOT NULL DEFAULT '',
                        scopes TEXT NOT NULL DEFAULT '',
                        enabled BOOLEAN NOT NULL DEFAULT TRUE
                    );
                    CREATE TABLE IF NOT EXISTS group_role_mappings (
                        id TEXT PRIMARY KEY,
                        provider_id TEXT NOT NULL,
                        external_group TEXT NOT NULL,
                        local_roles_csv TEXT NOT NULL,
                        enabled BOOLEAN NOT NULL DEFAULT TRUE,
                        priority BIGINT NOT NULL DEFAULT 0
                    );
                    CREATE TABLE IF NOT EXISTS idempotency_records (
                        scope_key TEXT PRIMARY KEY,
                        request_hash TEXT NOT NULL,
                        state TEXT NOT NULL CHECK (state IN (
                            'pending', 'claimed', 'dispatched', 'completed', 'indeterminate',
                            'completed_by_operator', 'released_by_operator'
                        )),
                        owner_attempt_id TEXT NOT NULL,
                        status_code BIGINT NOT NULL DEFAULT 0,
                        headers_json TEXT NOT NULL DEFAULT '{}',
                        body BYTEA NOT NULL DEFAULT ''::bytea,
                        created_at_ms BIGINT NOT NULL,
                        updated_at_ms BIGINT NOT NULL,
                        expires_at_ms BIGINT NOT NULL,
                        state_version BIGINT NOT NULL DEFAULT 1,
                        dispatched_at_ms BIGINT,
                        completed_at_ms BIGINT,
                        reconciled_by TEXT,
                        reconcile_reason TEXT,
                        result_hash TEXT
                    );
                    CREATE INDEX IF NOT EXISTS idx_idempotency_expires
                        ON idempotency_records(state, expires_at_ms);
                    "#,
                )
                .await?;
            // The checked-in migration is the authoritative PostgreSQL source
            // for audit/fault schema, indexes, and retention helpers.
            client
                .batch_execute(include_str!(
                    "../../../infra/migrations/postgres/002_audit_hardening.sql"
                ))
                .await?;
            client
                .batch_execute(include_str!(
                    "../../../infra/migrations/postgres/003_auth_version.sql"
                ))
                .await?;
            client
                .batch_execute(include_str!(
                    "../../../infra/migrations/postgres/004_idempotency_reconciliation.sql"
                ))
                .await?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_pool_budget_rejects_aggregate_overcommit() {
        let config = PostgresPoolConfig {
            max_size: 20,
            acquire_timeout: std::time::Duration::from_millis(100),
            connect_timeout: std::time::Duration::from_secs(1),
            query_timeout: std::time::Duration::from_secs(1),
        };

        assert!(validate_postgres_connection_budget(&config, 4, 10, 100).is_ok());
        assert!(validate_postgres_connection_budget(&config, 5, 10, 100).is_err());
    }

    #[test]
    fn postgres_pool_config_rejects_zero_or_unbounded_timeouts() {
        let mut config = PostgresPoolConfig::default();
        assert!(config.validate().is_ok());

        config.max_size = 0;
        assert!(config.validate().is_err());
        config.max_size = 1;
        config.acquire_timeout = std::time::Duration::ZERO;
        assert!(config.validate().is_err());
        config.acquire_timeout = std::time::Duration::from_secs(1);
        config.query_timeout = std::time::Duration::ZERO;
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn sqlite_health_check_executes_a_read_only_query() {
        let store = StorageStore::Sqlite(SqliteStore::new(":memory:"));
        store.health_check().await.unwrap();
    }
}
