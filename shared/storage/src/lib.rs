//! Shared SQLite/PostgreSQL persistence for Edge control, audit, and idempotency state.

mod api_routes;
mod app_metrics;
mod apps;
mod audit_logs;
mod audit_writer;
mod error;
mod fault_events;
mod idempotency;
mod identity;
mod paths;
mod policies;
mod routes;
mod sqlite_store;
mod store;
mod users;

pub use api_routes::{ApiRouteRecord, ApiRoutesStore};
pub use app_metrics::{AppMetricMinuteRecord, AppMetricsStore};
pub use apps::{AppRecord, AppsStore};
pub use audit_logs::{AuditLogFilter, AuditLogRecord, AuditLogsStore, SecurityMutation};
pub use audit_writer::{AuditEnqueueError, AuditShutdownReport, AuditWriter, AuditWriterConfig};
pub use error::StorageError;
pub use fault_events::{FaultEventFilter, FaultEventRecord, FaultEventsStore};
pub use idempotency::{IdempotencyClaim, IdempotencyRecord, IdempotencyState, IdempotencyStore};
pub use identity::{GroupRoleMappingRecord, IdentityProviderRecord, IdentityStore};
pub use paths::{
    ensure_storage_dir_for_path, resolve_storage_db_path, DEFAULT_STORAGE_DB_REL_PATH,
};
pub use policies::{PoliciesStore, PolicyEffect, PolicyRecord};
pub use routes::{IntranetUpstreamRecord, RoutesStore, TunnelRouteRecord};
pub use sqlite_store::SqliteStore;
pub use store::{
    build_store_from_env, ensure_store_schema, redact_postgres_dsn, resolve_postgres_dsn,
    resolve_storage_backend, validate_postgres_connection_budget, PostgresPoolConfig,
    PostgresStore, StorageBackend, StorageStore,
};
pub use users::{UserRecord, UsersStore};
