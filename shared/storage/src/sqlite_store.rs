use std::sync::Arc;

use crate::StorageError;

#[derive(Clone)]
pub struct SqliteStore {
    path: Arc<String>,
}

impl SqliteStore {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: Arc::new(path.into()),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub async fn health_check(&self) -> Result<(), StorageError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let connection = rusqlite::Connection::open(path.as_str())?;
            connection.query_row("SELECT 1", [], |_row| Ok(()))?;
            Ok::<(), StorageError>(())
        })
        .await
        .map_err(|error| StorageError::Task(error.to_string()))??;
        Ok(())
    }

    /// Creates tables if missing (idempotent).
    pub async fn ensure_schema(&self) -> Result<(), StorageError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(path.as_str())?;
            conn.execute_batch(
                r"
                PRAGMA foreign_keys = ON;
                CREATE TABLE IF NOT EXISTS tunnel_routes (
                    host TEXT PRIMARY KEY,
                    app_id TEXT NOT NULL,
                    connector_endpoint TEXT NOT NULL,
                    require_healthy_tunnel INTEGER NOT NULL DEFAULT 1
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
                    enabled INTEGER NOT NULL DEFAULT 1,
                    auth_version INTEGER NOT NULL DEFAULT 1,
                    updated_at_ms INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS app_metrics_minute (
                    ts_minute INTEGER NOT NULL,
                    app_id TEXT NOT NULL,
                    request_count INTEGER NOT NULL DEFAULT 0,
                    pv_count INTEGER NOT NULL DEFAULT 0,
                    uv_count INTEGER NOT NULL DEFAULT 0,
                    unique_ip_count INTEGER NOT NULL DEFAULT 0,
                    err4xx_count INTEGER NOT NULL DEFAULT 0,
                    err5xx_count INTEGER NOT NULL DEFAULT 0,
                    qps_avg REAL NOT NULL DEFAULT 0,
                    PRIMARY KEY (ts_minute, app_id)
                );
                CREATE TABLE IF NOT EXISTS apps (
                    app_id TEXT PRIMARY KEY,
                    display_name TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '',
                    enabled INTEGER NOT NULL DEFAULT 1
                );
                CREATE TABLE IF NOT EXISTS api_routes (
                    id TEXT PRIMARY KEY,
                    app_id TEXT NOT NULL,
                    method TEXT NOT NULL,
                    path TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    description TEXT NOT NULL DEFAULT ''
                );
                CREATE TABLE IF NOT EXISTS identity_providers (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    issuer TEXT NOT NULL DEFAULT '',
                    client_id TEXT NOT NULL DEFAULT '',
                    client_secret TEXT NOT NULL DEFAULT '',
                    scopes TEXT NOT NULL DEFAULT '',
                    enabled INTEGER NOT NULL DEFAULT 1
                );
                CREATE TABLE IF NOT EXISTS group_role_mappings (
                    id TEXT PRIMARY KEY,
                    provider_id TEXT NOT NULL,
                    external_group TEXT NOT NULL,
                    local_roles_csv TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    priority INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS audit_logs (
                    id TEXT PRIMARY KEY,
                    ts_ms INTEGER NOT NULL,
                    service TEXT NOT NULL,
                    user_id TEXT NOT NULL DEFAULT '',
                    app_id TEXT NOT NULL DEFAULT '',
                    path TEXT NOT NULL DEFAULT '',
                    method TEXT NOT NULL DEFAULT '',
                    latency_ms INTEGER NOT NULL DEFAULT 0,
                    decision TEXT NOT NULL DEFAULT '',
                    result TEXT NOT NULL DEFAULT '',
                    trace_id TEXT NOT NULL DEFAULT '',
                    extra_json TEXT NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS idx_audit_logs_ts_ms
                    ON audit_logs(ts_ms);
                CREATE INDEX IF NOT EXISTS idx_audit_logs_service_ts_ms
                    ON audit_logs(service, ts_ms);
                CREATE INDEX IF NOT EXISTS idx_audit_logs_user_ts_ms
                    ON audit_logs(user_id, ts_ms);
                CREATE INDEX IF NOT EXISTS idx_audit_logs_app_ts_ms
                    ON audit_logs(app_id, ts_ms);
                CREATE INDEX IF NOT EXISTS idx_audit_logs_trace_id
                    ON audit_logs(trace_id);
                CREATE TABLE IF NOT EXISTS fault_events (
                    id TEXT PRIMARY KEY,
                    ts_ms INTEGER NOT NULL,
                    service TEXT NOT NULL,
                    event_type TEXT NOT NULL,
                    severity TEXT NOT NULL DEFAULT 'warn',
                    path TEXT NOT NULL DEFAULT '',
                    method TEXT NOT NULL DEFAULT '',
                    latency_ms INTEGER NOT NULL DEFAULT 0,
                    baseline_ms INTEGER NOT NULL DEFAULT 0,
                    threshold_ms INTEGER NOT NULL DEFAULT 0,
                    status_code INTEGER NOT NULL DEFAULT 0,
                    result TEXT NOT NULL DEFAULT '',
                    trace_id TEXT NOT NULL DEFAULT '',
                    source TEXT NOT NULL DEFAULT 'detector',
                    resolved_at_ms INTEGER,
                    meta_json TEXT NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS idx_fault_events_ts_ms
                    ON fault_events(ts_ms);
                CREATE INDEX IF NOT EXISTS idx_fault_events_service_ts_ms
                    ON fault_events(service, ts_ms);
                CREATE INDEX IF NOT EXISTS idx_fault_events_trace_id
                    ON fault_events(trace_id);
                CREATE TABLE IF NOT EXISTS idempotency_records (
                    scope_key TEXT PRIMARY KEY,
                    request_hash TEXT NOT NULL,
                    state TEXT NOT NULL CHECK (state IN (
                        'pending', 'claimed', 'dispatched', 'completed', 'indeterminate',
                        'completed_by_operator', 'released_by_operator'
                    )),
                    owner_attempt_id TEXT NOT NULL,
                    status_code INTEGER NOT NULL DEFAULT 0,
                    headers_json TEXT NOT NULL DEFAULT '{}',
                    body BLOB NOT NULL DEFAULT X'',
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    expires_at_ms INTEGER NOT NULL,
                    state_version INTEGER NOT NULL DEFAULT 1,
                    dispatched_at_ms INTEGER,
                    completed_at_ms INTEGER,
                    reconciled_by TEXT,
                    reconcile_reason TEXT,
                    result_hash TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_idempotency_expires
                    ON idempotency_records(state, expires_at_ms);
                CREATE INDEX IF NOT EXISTS idx_idempotency_reconciliation_queue
                    ON idempotency_records(state, updated_at_ms);
                CREATE TABLE IF NOT EXISTS idempotency_reconciliation_events (
                    event_id TEXT PRIMARY KEY,
                    scope_key TEXT NOT NULL,
                    previous_state TEXT NOT NULL,
                    new_state TEXT NOT NULL,
                    previous_version INTEGER NOT NULL,
                    new_version INTEGER NOT NULL,
                    reconciled_by TEXT NOT NULL,
                    reconcile_reason TEXT NOT NULL,
                    result_hash TEXT,
                    created_at_ms INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_idempotency_reconciliation_events_scope
                    ON idempotency_reconciliation_events(scope_key, created_at_ms);
                ",
            )?;
            let mut has_display_name = false;
            let mut has_title = false;
            let mut has_auth_version = false;
            let mut has_updated_at_ms = false;
            let mut stmt = conn.prepare("PRAGMA table_info(users)")?;
            let cols = stmt.query_map([], |row| row.get::<_, String>(1))?;
            for col in cols {
                let c = col?;
                if c == "display_name" {
                    has_display_name = true;
                }
                if c == "title" {
                    has_title = true;
                }
                if c == "auth_version" {
                    has_auth_version = true;
                }
                if c == "updated_at_ms" {
                    has_updated_at_ms = true;
                }
            }
            if !has_display_name {
                conn.execute("ALTER TABLE users ADD COLUMN display_name TEXT", [])?;
            }
            if !has_title {
                conn.execute("ALTER TABLE users ADD COLUMN title TEXT", [])?;
            }
            if !has_auth_version {
                conn.execute(
                    "ALTER TABLE users ADD COLUMN auth_version INTEGER NOT NULL DEFAULT 1",
                    [],
                )?;
            }
            if !has_updated_at_ms {
                conn.execute(
                    "ALTER TABLE users ADD COLUMN updated_at_ms INTEGER NOT NULL DEFAULT 0",
                    [],
                )?;
            }
            let has_idempotency_state_version = {
                let mut statement = conn.prepare("PRAGMA table_info(idempotency_records)")?;
                let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
                let mut found = false;
                for column in columns {
                    if column? == "state_version" {
                        found = true;
                    }
                }
                found
            };
            if !has_idempotency_state_version {
                conn.execute_batch(
                    r"
                    DROP INDEX IF EXISTS idx_idempotency_expires;
                    DROP INDEX IF EXISTS idx_idempotency_reconciliation_queue;
                    ALTER TABLE idempotency_records RENAME TO idempotency_records_legacy;
                    CREATE TABLE idempotency_records (
                        scope_key TEXT PRIMARY KEY,
                        request_hash TEXT NOT NULL,
                        state TEXT NOT NULL CHECK (state IN (
                            'pending', 'claimed', 'dispatched', 'completed', 'indeterminate',
                            'completed_by_operator', 'released_by_operator'
                        )),
                        owner_attempt_id TEXT NOT NULL,
                        status_code INTEGER NOT NULL DEFAULT 0,
                        headers_json TEXT NOT NULL DEFAULT '{}',
                        body BLOB NOT NULL DEFAULT X'',
                        created_at_ms INTEGER NOT NULL,
                        updated_at_ms INTEGER NOT NULL,
                        expires_at_ms INTEGER NOT NULL,
                        state_version INTEGER NOT NULL DEFAULT 1,
                        dispatched_at_ms INTEGER,
                        completed_at_ms INTEGER,
                        reconciled_by TEXT,
                        reconcile_reason TEXT,
                        result_hash TEXT
                    );
                    INSERT INTO idempotency_records (
                        scope_key, request_hash, state, owner_attempt_id, status_code,
                        headers_json, body, created_at_ms, updated_at_ms, expires_at_ms,
                        state_version, completed_at_ms, result_hash
                    )
                    SELECT scope_key, request_hash,
                           CASE WHEN state = 'pending' THEN 'indeterminate' ELSE state END,
                           owner_attempt_id, status_code, headers_json, body, created_at_ms,
                           updated_at_ms, expires_at_ms, 1,
                           CASE WHEN state = 'completed' THEN updated_at_ms ELSE NULL END,
                           NULL
                    FROM idempotency_records_legacy;
                    DROP TABLE idempotency_records_legacy;
                    CREATE INDEX idx_idempotency_expires
                        ON idempotency_records(state, expires_at_ms);
                    CREATE INDEX idx_idempotency_reconciliation_queue
                        ON idempotency_records(state, updated_at_ms);
                    ",
                )?;
            }
            Ok::<_, StorageError>(())
        })
        .await
        .map_err(|e| StorageError::Task(e.to_string()))?
    }
}
