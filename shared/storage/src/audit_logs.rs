use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::{
    ApiRouteRecord, AppRecord, ConfigSyncJobDraft, ConfigSyncOperation, ConfigSyncStore,
    GroupRoleMappingRecord, IdentityProviderRecord, IntranetUpstreamRecord, PolicyEffect,
    PolicyRecord, StorageError, StorageStore, TunnelRouteRecord, UserRecord,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogRecord {
    pub id: String,
    pub ts_ms: i64,
    pub service: String,
    pub user_id: String,
    pub app_id: String,
    pub path: String,
    pub method: String,
    pub latency_ms: i64,
    pub decision: String,
    pub result: String,
    pub trace_id: String,
    pub extra_json: String,
}

impl AuditLogRecord {
    pub fn management(
        service: impl Into<String>,
        user_id: impl Into<String>,
        app_id: impl Into<String>,
        path: impl Into<String>,
        method: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or(0),
            service: service.into(),
            user_id: user_id.into(),
            app_id: app_id.into(),
            path: path.into(),
            method: method.into(),
            latency_ms: 0,
            decision: "MUTATE".into(),
            result: "COMMITTED".into(),
            trace_id: uuid::Uuid::new_v4().to_string(),
            extra_json: "{}".into(),
        }
    }
}

#[cfg(test)]
mod transaction_tests {
    use std::collections::HashSet;

    use super::*;
    use crate::{
        ensure_store_schema, AppRecord, AppsStore, SecurityMutation, SqliteStore, StorageStore,
    };

    fn row(id: &str) -> AuditLogRecord {
        AuditLogRecord {
            id: id.into(),
            ts_ms: 1,
            service: "test-admin".into(),
            user_id: "admin".into(),
            app_id: "app-rollback".into(),
            path: "/api/v1/apps".into(),
            method: "POST".into(),
            latency_ms: 0,
            decision: "MUTATE".into(),
            result: "CREATED".into(),
            trace_id: "trace-rollback".into(),
            extra_json: "{}".into(),
        }
    }

    #[tokio::test]
    async fn security_mutation_rolls_back_when_audit_insert_fails() {
        let path = std::env::temp_dir().join(format!(
            "sag-audit-transaction-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = StorageStore::Sqlite(SqliteStore::new(path.to_string_lossy().to_string()));
        ensure_store_schema(&store).await.unwrap();
        let audit = row("duplicate-audit-id");
        AuditLogsStore::insert(&store, &audit).await.unwrap();

        let result = AuditLogsStore::apply_security_mutation(
            &store,
            &SecurityMutation::UpsertApp(AppRecord {
                app_id: "app-rollback".into(),
                display_name: "must roll back".into(),
                description: String::new(),
                enabled: true,
            }),
            &audit,
        )
        .await;

        assert!(result.is_err());
        assert!(AppsStore::load_all(&store).await.unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn management_audit_ids_are_unique_uuid_v4() {
        let ids = (0..10_000)
            .map(|_| {
                AuditLogRecord::management("test-admin", "user-1", "app-1", "/api/v1/test", "POST")
                    .id
            })
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), 10_000);
        assert!(ids.iter().all(|id| {
            uuid::Uuid::parse_str(id)
                .map(|uuid| uuid.get_version() == Some(uuid::Version::Random))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn route_configuration_snapshot_rejects_unresolvable_shapes() {
        let valid_route = TunnelRouteRecord {
            host: "app.internal".into(),
            app_id: "app-1".into(),
            connector_endpoint: "connector-local-001:stream".into(),
            require_healthy_tunnel: true,
        };
        for upstream in [
            "bad!host:8080",
            "[not-an-ip]:8080",
            "missing-port",
            "host:0",
            "https://host:443/path",
        ] {
            let error = validate_route_configuration_snapshot(
                std::slice::from_ref(&valid_route),
                &[IntranetUpstreamRecord {
                    app_id: "app-1".into(),
                    upstream: upstream.into(),
                    scheme: "http".into(),
                }],
            )
            .unwrap_err();
            assert!(matches!(error, StorageError::Validation(_)), "{upstream}");
        }

        for (host, connector_endpoint) in
            [("bad!host", "connector:stream"), ("app.internal", "::::")]
        {
            let error = validate_route_configuration_snapshot(
                &[TunnelRouteRecord {
                    host: host.into(),
                    connector_endpoint: connector_endpoint.into(),
                    ..valid_route.clone()
                }],
                &[],
            )
            .unwrap_err();
            assert!(matches!(error, StorageError::Validation(_)));
        }

        let uppercase_scheme = validate_route_configuration_snapshot(
            std::slice::from_ref(&valid_route),
            &[IntranetUpstreamRecord {
                app_id: "app-1".into(),
                upstream: "upstream.internal:8443".into(),
                scheme: "HTTPS".into(),
            }],
        )
        .unwrap_err();
        assert!(matches!(uppercase_scheme, StorageError::Validation(_)));
    }

    #[test]
    fn route_configuration_snapshot_accepts_dns_ipv4_and_ipv6_upstreams() {
        let route = TunnelRouteRecord {
            host: "app-1.internal".into(),
            app_id: "app-1".into(),
            connector_endpoint: "connector_1:stream.v1".into(),
            require_healthy_tunnel: true,
        };
        for upstream in [
            "apisix-upstream:8080",
            "127.0.0.1:8080",
            "[2001:db8::1]:8443",
        ] {
            validate_route_configuration_snapshot(
                std::slice::from_ref(&route),
                &[IntranetUpstreamRecord {
                    app_id: "app-1".into(),
                    upstream: upstream.into(),
                    scheme: "https".into(),
                }],
            )
            .unwrap();
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AuditLogFilter {
    pub from_ts_ms: Option<i64>,
    pub to_ts_ms: Option<i64>,
    pub user_id: Option<String>,
    pub app_id: Option<String>,
    pub limit: i64,
}

/// Security-relevant management mutations that must commit atomically with
/// their audit record. Data-plane observations use [`crate::AuditWriter`]
/// instead and never block a request on storage.
#[derive(Debug, Clone)]
pub enum SecurityMutation {
    UpsertUser(UserRecord),
    DeleteUser(String),
    UpsertIdentityProvider(IdentityProviderRecord),
    DeleteIdentityProvider(String),
    UpsertGroupRoleMapping(GroupRoleMappingRecord),
    DeleteGroupRoleMapping(String),
    UpsertPolicy(PolicyRecord),
    DeletePolicy(String),
    UpsertApp(AppRecord),
    DeleteApp(String),
    UpsertApiRoute(ApiRouteRecord),
    DeleteApiRoute(String),
    UpsertTunnelRoute(TunnelRouteRecord),
    DeleteTunnelRoute(String),
    UpsertIntranetUpstream(IntranetUpstreamRecord),
}

fn validate_config_token(name: &str, value: &str, max_len: usize) -> Result<(), StorageError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > max_len
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err(StorageError::Validation(format!(
            "{name} must be 1..={max_len} characters with no leading/trailing, whitespace, or control characters"
        )));
    }
    Ok(())
}

fn validate_upstream_authority(upstream: &str) -> Result<(), StorageError> {
    validate_config_token("upstream", upstream, 255)?;
    if ['/', '?', '#']
        .into_iter()
        .any(|character| upstream.contains(character))
        || upstream.contains("://")
    {
        return Err(StorageError::Validation(
            "upstream must be a host:port authority without a scheme or path".into(),
        ));
    }
    let (host, port, bracketed_ipv6) = if let Some(bracketed) = upstream.strip_prefix('[') {
        let (host, port) = bracketed.split_once("]:").ok_or_else(|| {
            StorageError::Validation("IPv6 upstream must use [address]:port".into())
        })?;
        (host, port, true)
    } else {
        let (host, port) = upstream.rsplit_once(':').ok_or_else(|| {
            StorageError::Validation("upstream must include an explicit port".into())
        })?;
        if host.contains(':') {
            return Err(StorageError::Validation(
                "IPv6 upstream must use [address]:port".into(),
            ));
        }
        (host, port, false)
    };
    if host.is_empty() || port.parse::<u16>().ok().is_none_or(|port| port == 0) {
        return Err(StorageError::Validation(
            "upstream must contain a non-empty host and port in 1..=65535".into(),
        ));
    }
    if bracketed_ipv6 {
        host.parse::<Ipv6Addr>().map_err(|_| {
            StorageError::Validation("bracketed upstream host must be a valid IPv6 address".into())
        })?;
    } else {
        validate_dns_or_ipv4_host("upstream host", host)?;
    }
    Ok(())
}

fn validate_dns_or_ipv4_host(name: &str, host: &str) -> Result<(), StorageError> {
    if host.parse::<Ipv4Addr>().is_ok() {
        return Ok(());
    }
    let valid_dns_name = host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        });
    if !valid_dns_name {
        return Err(StorageError::Validation(format!(
            "{name} must be a valid IPv4 address or DNS name"
        )));
    }
    Ok(())
}

fn validate_tunnel_route_shape(record: &TunnelRouteRecord) -> Result<(), StorageError> {
    validate_config_token("host", &record.host, 253)?;
    if ['/', '?', '#', '\\']
        .into_iter()
        .any(|character| record.host.contains(character))
        || record.host.contains("://")
    {
        return Err(StorageError::Validation(
            "host must not contain a scheme, path, query, or fragment".into(),
        ));
    }
    if let Some(ipv6) = record
        .host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        ipv6.parse::<Ipv6Addr>().map_err(|_| {
            StorageError::Validation("bracketed route host must be a valid IPv6 address".into())
        })?;
    } else {
        if record.host.contains(':') {
            return Err(StorageError::Validation(
                "IPv6 route host must use [address] notation".into(),
            ));
        }
        validate_dns_or_ipv4_host("host", &record.host)?;
    }

    validate_config_token("app_id", &record.app_id, 128)?;
    validate_config_token("connector_endpoint", &record.connector_endpoint, 255)?;
    let valid_connector_endpoint = record.connector_endpoint.split(':').all(|segment| {
        !segment.is_empty()
            && segment.bytes().any(|byte| byte.is_ascii_alphanumeric())
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    });
    if !valid_connector_endpoint {
        return Err(StorageError::Validation(
            "connector_endpoint must contain non-empty ':'-separated logical labels using ASCII letters, digits, '-', '_', or '.'"
                .into(),
        ));
    }
    Ok(())
}

fn validate_intranet_upstream_shape(record: &IntranetUpstreamRecord) -> Result<(), StorageError> {
    validate_config_token("app_id", &record.app_id, 128)?;
    validate_upstream_authority(&record.upstream)?;
    if !matches!(record.scheme.as_str(), "http" | "https") {
        return Err(StorageError::Validation(
            "scheme must be exactly http or https".into(),
        ));
    }
    Ok(())
}

/// Validates a complete desired route snapshot before bulk import or cutover.
/// This applies the same row-shape rules as online mutations and also enforces
/// the Agent invariant that every host for one app resolves to one Connector
/// endpoint and health policy.
pub fn validate_route_configuration_snapshot(
    routes: &[TunnelRouteRecord],
    upstreams: &[IntranetUpstreamRecord],
) -> Result<(), StorageError> {
    let mut app_connectors = BTreeMap::<&str, (&str, bool)>::new();
    for route in routes {
        validate_tunnel_route_shape(route)?;
        match app_connectors.get(route.app_id.as_str()) {
            Some((endpoint, require_healthy_tunnel))
                if *endpoint != route.connector_endpoint
                    || *require_healthy_tunnel != route.require_healthy_tunnel =>
            {
                return Err(StorageError::Conflict(format!(
                    "all routes for app_id {} must use the same connector_endpoint and require_healthy_tunnel value",
                    route.app_id
                )));
            }
            Some(_) => {}
            None => {
                app_connectors.insert(
                    route.app_id.as_str(),
                    (
                        route.connector_endpoint.as_str(),
                        route.require_healthy_tunnel,
                    ),
                );
            }
        }
    }
    for upstream in upstreams {
        validate_intranet_upstream_shape(upstream)?;
    }
    Ok(())
}

fn validate_config_mutation_shape(mutation: &SecurityMutation) -> Result<(), StorageError> {
    match mutation {
        SecurityMutation::UpsertTunnelRoute(record) => validate_tunnel_route_shape(record)?,
        SecurityMutation::DeleteTunnelRoute(host) => {
            validate_config_token("host", host, 253)?;
        }
        SecurityMutation::UpsertIntranetUpstream(record) => {
            validate_intranet_upstream_shape(record)?
        }
        _ => {}
    }
    Ok(())
}

fn is_route_config_mutation(mutation: &SecurityMutation) -> bool {
    matches!(
        mutation,
        SecurityMutation::UpsertTunnelRoute(_)
            | SecurityMutation::DeleteTunnelRoute(_)
            | SecurityMutation::UpsertIntranetUpstream(_)
    )
}

fn validate_sqlite_route_consistency(
    transaction: &rusqlite::Transaction<'_>,
    mutation: &SecurityMutation,
) -> Result<(), StorageError> {
    let SecurityMutation::UpsertTunnelRoute(record) = mutation else {
        return Ok(());
    };
    let conflicts: i64 = transaction.query_row(
        "SELECT EXISTS( \
           SELECT 1 FROM tunnel_routes \
           WHERE app_id = ?1 AND host <> ?2 \
             AND (connector_endpoint <> ?3 OR require_healthy_tunnel <> ?4) \
         )",
        rusqlite::params![
            record.app_id,
            record.host,
            record.connector_endpoint,
            i32::from(record.require_healthy_tunnel)
        ],
        |row| row.get(0),
    )?;
    if conflicts != 0 {
        return Err(StorageError::Conflict(format!(
            "all routes for app_id {} must use the same connector_endpoint and require_healthy_tunnel value",
            record.app_id
        )));
    }
    Ok(())
}

async fn lock_postgres_config_apps(
    transaction: &tokio_postgres::Transaction<'_>,
    mutation: &SecurityMutation,
    previous_route_app: Option<&str>,
) -> Result<(), StorageError> {
    for app_id in affected_config_apps(mutation, previous_route_app) {
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(\
                   hashtextextended(json_build_array('APISIX'::TEXT, $1::TEXT)::TEXT, 0)\
                 )",
                &[&app_id],
            )
            .await?;
    }
    Ok(())
}

async fn validate_postgres_route_consistency(
    transaction: &tokio_postgres::Transaction<'_>,
    mutation: &SecurityMutation,
) -> Result<(), StorageError> {
    let SecurityMutation::UpsertTunnelRoute(record) = mutation else {
        return Ok(());
    };
    let conflicts: bool = transaction
        .query_one(
            "SELECT EXISTS( \
               SELECT 1 FROM tunnel_routes \
               WHERE app_id = $1 AND host <> $2 \
                 AND (connector_endpoint <> $3 OR require_healthy_tunnel <> $4) \
             )",
            &[
                &record.app_id,
                &record.host,
                &record.connector_endpoint,
                &record.require_healthy_tunnel,
            ],
        )
        .await?
        .get(0);
    if conflicts {
        return Err(StorageError::Conflict(format!(
            "all routes for app_id {} must use the same connector_endpoint and require_healthy_tunnel value",
            record.app_id
        )));
    }
    Ok(())
}

fn affected_config_apps(
    mutation: &SecurityMutation,
    previous_route_app: Option<&str>,
) -> Vec<String> {
    let mut app_ids = match mutation {
        SecurityMutation::UpsertTunnelRoute(record) => {
            let mut app_ids = vec![record.app_id.clone()];
            if previous_route_app.is_some_and(|app_id| app_id != record.app_id) {
                app_ids.push(previous_route_app.unwrap().to_string());
            }
            app_ids
        }
        SecurityMutation::DeleteTunnelRoute(_) => previous_route_app
            .map(|app_id| vec![app_id.to_string()])
            .unwrap_or_default(),
        SecurityMutation::UpsertIntranetUpstream(record) => vec![record.app_id.clone()],
        _ => Vec::new(),
    };
    app_ids.sort();
    app_ids.dedup();
    app_ids
}

fn previous_sqlite_route_app(
    transaction: &rusqlite::Transaction<'_>,
    mutation: &SecurityMutation,
) -> Result<Option<String>, StorageError> {
    use rusqlite::OptionalExtension;

    let host = match mutation {
        SecurityMutation::UpsertTunnelRoute(record) => Some(record.host.as_str()),
        SecurityMutation::DeleteTunnelRoute(host) => Some(host.as_str()),
        _ => None,
    };
    let Some(host) = host else {
        return Ok(None);
    };
    transaction
        .query_row(
            "SELECT app_id FROM tunnel_routes WHERE host = ?1",
            [host],
            |row| row.get(0),
        )
        .optional()
        .map_err(StorageError::from)
}

async fn previous_postgres_route_app(
    transaction: &tokio_postgres::Transaction<'_>,
    mutation: &SecurityMutation,
) -> Result<Option<String>, StorageError> {
    let host = match mutation {
        SecurityMutation::UpsertTunnelRoute(record) => Some(record.host.as_str()),
        SecurityMutation::DeleteTunnelRoute(host) => Some(host.as_str()),
        _ => None,
    };
    let Some(host) = host else {
        return Ok(None);
    };
    // Serialize ownership changes even when the host row does not exist yet.
    // A row-level FOR UPDATE cannot fence concurrent inserts, while this
    // transaction-scoped advisory lock covers both insert and move cases.
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&host],
        )
        .await?;
    Ok(transaction
        .query_opt("SELECT app_id FROM tunnel_routes WHERE host = $1", &[&host])
        .await?
        .map(|row| row.get(0)))
}

fn apply_sqlite_config_convergence(
    transaction: &rusqlite::Transaction<'_>,
    mutation: &SecurityMutation,
    previous_route_app: Option<&str>,
    changed_at_ms: i64,
) -> Result<(), StorageError> {
    let app_ids = affected_config_apps(mutation, previous_route_app);
    if app_ids.is_empty() {
        return Ok(());
    }
    let generation =
        ConfigSyncStore::bump_generation_sqlite_transaction(transaction, changed_at_ms)?;
    for app_id in app_ids {
        let (has_route, has_upstream): (i64, i64) = transaction.query_row(
            "SELECT \
               EXISTS(SELECT 1 FROM tunnel_routes WHERE app_id = ?1), \
               EXISTS(SELECT 1 FROM intranet_upstreams WHERE app_id = ?1)",
            [&app_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let operation = if has_route != 0 && has_upstream != 0 {
            ConfigSyncOperation::Upsert
        } else {
            ConfigSyncOperation::Delete
        };
        ConfigSyncStore::enqueue_sqlite_transaction(
            transaction,
            &ConfigSyncJobDraft {
                generation,
                target: "APISIX".into(),
                resource_type: "ROUTE".into(),
                resource_id: app_id.clone(),
                app_id,
                operation,
                payload_json: None,
                next_attempt_at_ms: changed_at_ms,
            },
            changed_at_ms,
        )?;
    }
    Ok(())
}

async fn apply_postgres_config_convergence(
    transaction: &tokio_postgres::Transaction<'_>,
    mutation: &SecurityMutation,
    previous_route_app: Option<&str>,
    changed_at_ms: i64,
) -> Result<(), StorageError> {
    let app_ids = affected_config_apps(mutation, previous_route_app);
    if app_ids.is_empty() {
        return Ok(());
    }
    let generation =
        ConfigSyncStore::bump_generation_postgres_transaction(transaction, changed_at_ms).await?;
    for app_id in app_ids {
        let row = transaction
            .query_one(
                "SELECT \
                   EXISTS(SELECT 1 FROM tunnel_routes WHERE app_id = $1), \
                   EXISTS(SELECT 1 FROM intranet_upstreams WHERE app_id = $1)",
                &[&app_id],
            )
            .await?;
        let operation = if row.get::<_, bool>(0) && row.get::<_, bool>(1) {
            ConfigSyncOperation::Upsert
        } else {
            ConfigSyncOperation::Delete
        };
        ConfigSyncStore::enqueue_postgres_transaction(
            transaction,
            &ConfigSyncJobDraft {
                generation,
                target: "APISIX".into(),
                resource_type: "ROUTE".into(),
                resource_id: app_id.clone(),
                app_id,
                operation,
                payload_json: None,
                next_attempt_at_ms: changed_at_ms,
            },
            changed_at_ms,
        )
        .await?;
    }
    Ok(())
}

pub struct AuditLogsStore;

impl AuditLogsStore {
    pub async fn apply_security_mutation(
        store: &StorageStore,
        mutation: &SecurityMutation,
        audit: &AuditLogRecord,
    ) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let mutation = mutation.clone();
                let audit = audit.clone();
                tokio::task::spawn_blocking(move || {
                    let mut connection = rusqlite::Connection::open(sqlite.path())?;
                    let transaction = connection
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    validate_config_mutation_shape(&mutation)?;
                    let previous_route_app = previous_sqlite_route_app(&transaction, &mutation)?;
                    validate_sqlite_route_consistency(&transaction, &mutation)?;
                    execute_sqlite_security_mutation(&transaction, &mutation)?;
                    apply_sqlite_config_convergence(
                        &transaction,
                        &mutation,
                        previous_route_app.as_deref(),
                        audit.ts_ms,
                    )?;
                    insert_sqlite_audit_strict(&transaction, &audit)?;
                    transaction.commit()?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let mut client = postgres.client().await?;
                let transaction = client.transaction().await?;
                validate_config_mutation_shape(mutation)?;
                if is_route_config_mutation(mutation) {
                    // Generation increments serialize eventually; take the
                    // same singleton row lock before validation so two
                    // replicas cannot both validate incompatible app routes
                    // against a snapshot that excludes the other's insert.
                    transaction
                        .query_one(
                            "SELECT generation FROM config_state WHERE id = 1 FOR UPDATE",
                            &[],
                        )
                        .await?;
                }
                let previous_route_app =
                    previous_postgres_route_app(&transaction, mutation).await?;
                lock_postgres_config_apps(&transaction, mutation, previous_route_app.as_deref())
                    .await?;
                validate_postgres_route_consistency(&transaction, mutation).await?;
                execute_postgres_security_mutation(&transaction, mutation).await?;
                apply_postgres_config_convergence(
                    &transaction,
                    mutation,
                    previous_route_app.as_deref(),
                    audit.ts_ms,
                )
                .await?;
                insert_postgres_audit_strict(&transaction, audit).await?;
                transaction.commit().await?;
                Ok(())
            }
        }
    }

    pub async fn insert(store: &StorageStore, row: &AuditLogRecord) -> Result<(), StorageError> {
        Self::insert_batch(store, std::slice::from_ref(row)).await
    }

    pub async fn insert_batch(
        store: &StorageStore,
        rows: &[AuditLogRecord],
    ) -> Result<(), StorageError> {
        if rows.is_empty() {
            return Ok(());
        }
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let rows = rows.to_vec();
                tokio::task::spawn_blocking(move || {
                    let mut conn = rusqlite::Connection::open(sqlite.path())?;
                    let transaction = conn.transaction()?;
                    for row in rows {
                        transaction.execute(
                            r#"INSERT OR IGNORE INTO audit_logs
                               (id, ts_ms, service, user_id, app_id, path, method, latency_ms, decision, result, trace_id, extra_json)
                               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
                            rusqlite::params![
                                row.id, row.ts_ms, row.service, row.user_id, row.app_id, row.path,
                                row.method, row.latency_ms, row.decision, row.result, row.trace_id,
                                row.extra_json
                            ],
                        )?;
                    }
                    transaction.commit()?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let mut client = pg.client().await?;
                let transaction = client.transaction().await?;
                let statement = transaction
                    .prepare_cached(
                        r#"INSERT INTO audit_logs
                           (id, ts_ms, service, user_id, app_id, path, method, latency_ms, decision, result, trace_id, extra_json)
                           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                           ON CONFLICT (id) DO NOTHING"#,
                    )
                    .await?;
                for row in rows {
                    transaction
                        .execute(
                            &statement,
                            &[
                                &row.id,
                                &row.ts_ms,
                                &row.service,
                                &row.user_id,
                                &row.app_id,
                                &row.path,
                                &row.method,
                                &row.latency_ms,
                                &row.decision,
                                &row.result,
                                &row.trace_id,
                                &row.extra_json,
                            ],
                        )
                        .await?;
                }
                transaction.commit().await?;
                Ok(())
            }
        }
    }

    pub async fn list(
        store: &StorageStore,
        f: &AuditLogFilter,
    ) -> Result<Vec<AuditLogRecord>, StorageError> {
        let limit = if f.limit <= 0 { 200 } else { f.limit.min(1000) };
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let ff = f.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let mut conds = Vec::<String>::new();
                    let mut vals = Vec::<rusqlite::types::Value>::new();
                    if let Some(v) = ff.from_ts_ms { conds.push("ts_ms >= ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.to_ts_ms { conds.push("ts_ms <= ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.user_id { conds.push("user_id = ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.app_id { conds.push("app_id = ?".into()); vals.push(v.into()); }
                    let mut sql = "SELECT id, ts_ms, service, user_id, app_id, path, method, latency_ms, decision, result, trace_id, extra_json FROM audit_logs".to_string();
                    if !conds.is_empty() {
                        sql.push_str(" WHERE ");
                        sql.push_str(&conds.join(" AND "));
                    }
                    sql.push_str(" ORDER BY ts_ms DESC LIMIT ?");
                    vals.push(limit.into());
                    let mut stmt = conn.prepare(&sql)?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(vals), |r| {
                        Ok(AuditLogRecord {
                            id: r.get(0)?, ts_ms: r.get(1)?, service: r.get(2)?, user_id: r.get(3)?,
                            app_id: r.get(4)?, path: r.get(5)?, method: r.get(6)?, latency_ms: r.get(7)?,
                            decision: r.get(8)?, result: r.get(9)?, trace_id: r.get(10)?, extra_json: r.get(11)?,
                        })
                    })?;
                    let mut out = Vec::new();
                    for r in rows { out.push(r?); }
                    Ok::<_, StorageError>(out)
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let sql = r#"
                    SELECT id, ts_ms, service, user_id, app_id, path, method, latency_ms, decision, result, trace_id, extra_json
                    FROM audit_logs
                    WHERE ($1::bigint IS NULL OR ts_ms >= $1)
                      AND ($2::bigint IS NULL OR ts_ms <= $2)
                      AND ($3::text IS NULL OR user_id = $3)
                      AND ($4::text IS NULL OR app_id = $4)
                    ORDER BY ts_ms DESC
                    LIMIT $5
                "#;
                let rows = client
                    .query(
                        sql,
                        &[&f.from_ts_ms, &f.to_ts_ms, &f.user_id, &f.app_id, &limit],
                    )
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|r| AuditLogRecord {
                        id: r.get(0),
                        ts_ms: r.get(1),
                        service: r.get(2),
                        user_id: r.get(3),
                        app_id: r.get(4),
                        path: r.get(5),
                        method: r.get(6),
                        latency_ms: r.get(7),
                        decision: r.get(8),
                        result: r.get(9),
                        trace_id: r.get(10),
                        extra_json: r.get(11),
                    })
                    .collect())
            }
        }
    }
}

fn insert_sqlite_audit_strict(
    transaction: &rusqlite::Transaction<'_>,
    row: &AuditLogRecord,
) -> Result<(), StorageError> {
    transaction.execute(
        r#"INSERT INTO audit_logs
           (id, ts_ms, service, user_id, app_id, path, method, latency_ms, decision, result, trace_id, extra_json)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
        rusqlite::params![
            row.id,
            row.ts_ms,
            row.service,
            row.user_id,
            row.app_id,
            row.path,
            row.method,
            row.latency_ms,
            row.decision,
            row.result,
            row.trace_id,
            row.extra_json
        ],
    )?;
    Ok(())
}

async fn insert_postgres_audit_strict(
    transaction: &tokio_postgres::Transaction<'_>,
    row: &AuditLogRecord,
) -> Result<(), StorageError> {
    transaction
        .execute(
            r#"INSERT INTO audit_logs
               (id, ts_ms, service, user_id, app_id, path, method, latency_ms, decision, result, trace_id, extra_json)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)"#,
            &[
                &row.id,
                &row.ts_ms,
                &row.service,
                &row.user_id,
                &row.app_id,
                &row.path,
                &row.method,
                &row.latency_ms,
                &row.decision,
                &row.result,
                &row.trace_id,
                &row.extra_json,
            ],
        )
        .await?;
    Ok(())
}

fn execute_sqlite_security_mutation(
    transaction: &rusqlite::Transaction<'_>,
    mutation: &SecurityMutation,
) -> Result<(), StorageError> {
    match mutation {
        SecurityMutation::UpsertUser(record) => {
            let roles = serde_json::to_string(&record.roles)?;
            let updated_at_ms = auth_updated_at_ms().max(record.updated_at_ms);
            transaction.execute(
                r#"INSERT INTO users (id, username, password_hash, roles_json, display_name, title, enabled, auth_version, updated_at_ms)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                   ON CONFLICT(username) DO UPDATE SET id=excluded.id, password_hash=excluded.password_hash,
                     roles_json=excluded.roles_json, display_name=excluded.display_name,
                     title=excluded.title, enabled=excluded.enabled,
                     auth_version=users.auth_version + 1, updated_at_ms=excluded.updated_at_ms"#,
                rusqlite::params![record.id, record.username, record.password_hash, roles,
                    record.display_name, record.title, i32::from(record.enabled),
                    record.auth_version.max(1), updated_at_ms],
            )?;
        }
        SecurityMutation::DeleteUser(username) => {
            transaction.execute("DELETE FROM users WHERE username=?1", [username])?;
        }
        SecurityMutation::UpsertIdentityProvider(record) => {
            transaction.execute(
                r#"INSERT INTO identity_providers (id,kind,issuer,client_id,client_secret,scopes,enabled)
                   VALUES (?1,?2,?3,?4,?5,?6,?7)
                   ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, issuer=excluded.issuer,
                     client_id=excluded.client_id, client_secret=excluded.client_secret,
                     scopes=excluded.scopes, enabled=excluded.enabled"#,
                rusqlite::params![record.id, record.kind, record.issuer, record.client_id,
                    record.client_secret, record.scopes, i32::from(record.enabled)],
            )?;
        }
        SecurityMutation::DeleteIdentityProvider(id) => {
            transaction.execute("DELETE FROM identity_providers WHERE id=?1", [id])?;
        }
        SecurityMutation::UpsertGroupRoleMapping(record) => {
            transaction.execute(
                r#"INSERT INTO group_role_mappings
                   (id,provider_id,external_group,local_roles_csv,enabled,priority)
                   VALUES (?1,?2,?3,?4,?5,?6)
                   ON CONFLICT(id) DO UPDATE SET provider_id=excluded.provider_id,
                     external_group=excluded.external_group, local_roles_csv=excluded.local_roles_csv,
                     enabled=excluded.enabled, priority=excluded.priority"#,
                rusqlite::params![record.id, record.provider_id, record.external_group,
                    record.local_roles_csv, i32::from(record.enabled), record.priority],
            )?;
        }
        SecurityMutation::DeleteGroupRoleMapping(id) => {
            transaction.execute("DELETE FROM group_role_mappings WHERE id=?1", [id])?;
        }
        SecurityMutation::UpsertPolicy(record) => {
            let subjects = serde_json::to_string(&record.subjects)?;
            let effect = match record.effect {
                PolicyEffect::Allow => "ALLOW",
                PolicyEffect::Deny => "DENY",
            };
            transaction.execute(
                r#"INSERT INTO policies (id,effect,subjects_json,app_id,path_prefix,priority)
                   VALUES (?1,?2,?3,?4,?5,?6)
                   ON CONFLICT(id) DO UPDATE SET effect=excluded.effect,
                     subjects_json=excluded.subjects_json, app_id=excluded.app_id,
                     path_prefix=excluded.path_prefix, priority=excluded.priority"#,
                rusqlite::params![
                    record.id,
                    effect,
                    subjects,
                    record.app_id,
                    record.path_prefix,
                    record.priority
                ],
            )?;
        }
        SecurityMutation::DeletePolicy(id) => {
            transaction.execute("DELETE FROM policies WHERE id=?1", [id])?;
        }
        SecurityMutation::UpsertApp(record) => {
            transaction.execute(
                r#"INSERT INTO apps (app_id,display_name,description,enabled)
                   VALUES (?1,?2,?3,?4)
                   ON CONFLICT(app_id) DO UPDATE SET display_name=excluded.display_name,
                     description=excluded.description, enabled=excluded.enabled"#,
                rusqlite::params![
                    record.app_id,
                    record.display_name,
                    record.description,
                    i32::from(record.enabled)
                ],
            )?;
        }
        SecurityMutation::DeleteApp(app_id) => {
            transaction.execute("DELETE FROM apps WHERE app_id=?1", [app_id])?;
        }
        SecurityMutation::UpsertApiRoute(record) => {
            transaction.execute(
                r#"INSERT INTO api_routes (id,app_id,method,path,enabled,description)
                   VALUES (?1,?2,?3,?4,?5,?6)
                   ON CONFLICT(id) DO UPDATE SET app_id=excluded.app_id, method=excluded.method,
                     path=excluded.path, enabled=excluded.enabled, description=excluded.description"#,
                rusqlite::params![record.id, record.app_id, record.method, record.path,
                    i32::from(record.enabled), record.description],
            )?;
        }
        SecurityMutation::DeleteApiRoute(id) => {
            transaction.execute("DELETE FROM api_routes WHERE id=?1", [id])?;
        }
        SecurityMutation::UpsertTunnelRoute(record) => {
            transaction.execute(
                r#"INSERT INTO tunnel_routes (host,app_id,connector_endpoint,require_healthy_tunnel)
                   VALUES (?1,?2,?3,?4)
                   ON CONFLICT(host) DO UPDATE SET app_id=excluded.app_id,
                     connector_endpoint=excluded.connector_endpoint,
                     require_healthy_tunnel=excluded.require_healthy_tunnel"#,
                rusqlite::params![
                    record.host,
                    record.app_id,
                    record.connector_endpoint,
                    i32::from(record.require_healthy_tunnel)
                ],
            )?;
        }
        SecurityMutation::DeleteTunnelRoute(host) => {
            transaction.execute("DELETE FROM tunnel_routes WHERE host=?1", [host])?;
        }
        SecurityMutation::UpsertIntranetUpstream(record) => {
            transaction.execute(
                r#"INSERT INTO intranet_upstreams (app_id,upstream,scheme)
                   VALUES (?1,?2,?3)
                   ON CONFLICT(app_id) DO UPDATE SET upstream=excluded.upstream,
                     scheme=excluded.scheme"#,
                rusqlite::params![record.app_id, record.upstream, record.scheme],
            )?;
        }
    }
    Ok(())
}

async fn execute_postgres_security_mutation(
    transaction: &tokio_postgres::Transaction<'_>,
    mutation: &SecurityMutation,
) -> Result<(), StorageError> {
    match mutation {
        SecurityMutation::UpsertUser(record) => {
            let roles = serde_json::to_string(&record.roles)?;
            let updated_at_ms = auth_updated_at_ms().max(record.updated_at_ms);
            transaction.execute(
                r#"INSERT INTO users (id,username,password_hash,roles_json,display_name,title,enabled,auth_version,updated_at_ms)
                   VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                   ON CONFLICT(username) DO UPDATE SET id=excluded.id,
                     password_hash=excluded.password_hash, roles_json=excluded.roles_json,
                     display_name=excluded.display_name, title=excluded.title, enabled=excluded.enabled,
                     auth_version=users.auth_version + 1, updated_at_ms=excluded.updated_at_ms"#,
                &[&record.id, &record.username, &record.password_hash, &roles,
                    &record.display_name, &record.title, &record.enabled,
                    &record.auth_version.max(1), &updated_at_ms],
            ).await?;
        }
        SecurityMutation::DeleteUser(username) => {
            transaction
                .execute("DELETE FROM users WHERE username=$1", &[username])
                .await?;
        }
        SecurityMutation::UpsertIdentityProvider(record) => {
            transaction.execute(
                r#"INSERT INTO identity_providers (id,kind,issuer,client_id,client_secret,scopes,enabled)
                   VALUES ($1,$2,$3,$4,$5,$6,$7)
                   ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, issuer=excluded.issuer,
                     client_id=excluded.client_id, client_secret=excluded.client_secret,
                     scopes=excluded.scopes, enabled=excluded.enabled"#,
                &[&record.id, &record.kind, &record.issuer, &record.client_id,
                    &record.client_secret, &record.scopes, &record.enabled],
            ).await?;
        }
        SecurityMutation::DeleteIdentityProvider(id) => {
            transaction
                .execute("DELETE FROM identity_providers WHERE id=$1", &[id])
                .await?;
        }
        SecurityMutation::UpsertGroupRoleMapping(record) => {
            transaction.execute(
                r#"INSERT INTO group_role_mappings
                   (id,provider_id,external_group,local_roles_csv,enabled,priority)
                   VALUES ($1,$2,$3,$4,$5,$6)
                   ON CONFLICT(id) DO UPDATE SET provider_id=excluded.provider_id,
                     external_group=excluded.external_group, local_roles_csv=excluded.local_roles_csv,
                     enabled=excluded.enabled, priority=excluded.priority"#,
                &[&record.id, &record.provider_id, &record.external_group,
                    &record.local_roles_csv, &record.enabled, &record.priority],
            ).await?;
        }
        SecurityMutation::DeleteGroupRoleMapping(id) => {
            transaction
                .execute("DELETE FROM group_role_mappings WHERE id=$1", &[id])
                .await?;
        }
        SecurityMutation::UpsertPolicy(record) => {
            let subjects = serde_json::to_string(&record.subjects)?;
            let effect = match record.effect {
                PolicyEffect::Allow => "ALLOW",
                PolicyEffect::Deny => "DENY",
            };
            transaction
                .execute(
                    r#"INSERT INTO policies (id,effect,subjects_json,app_id,path_prefix,priority)
                   VALUES ($1,$2,$3,$4,$5,$6)
                   ON CONFLICT(id) DO UPDATE SET effect=excluded.effect,
                     subjects_json=excluded.subjects_json, app_id=excluded.app_id,
                     path_prefix=excluded.path_prefix, priority=excluded.priority"#,
                    &[
                        &record.id,
                        &effect,
                        &subjects,
                        &record.app_id,
                        &record.path_prefix,
                        &record.priority,
                    ],
                )
                .await?;
        }
        SecurityMutation::DeletePolicy(id) => {
            transaction
                .execute("DELETE FROM policies WHERE id=$1", &[id])
                .await?;
        }
        SecurityMutation::UpsertApp(record) => {
            transaction
                .execute(
                    r#"INSERT INTO apps (app_id,display_name,description,enabled)
                   VALUES ($1,$2,$3,$4)
                   ON CONFLICT(app_id) DO UPDATE SET display_name=excluded.display_name,
                     description=excluded.description, enabled=excluded.enabled"#,
                    &[
                        &record.app_id,
                        &record.display_name,
                        &record.description,
                        &record.enabled,
                    ],
                )
                .await?;
        }
        SecurityMutation::DeleteApp(app_id) => {
            transaction
                .execute("DELETE FROM apps WHERE app_id=$1", &[app_id])
                .await?;
        }
        SecurityMutation::UpsertApiRoute(record) => {
            transaction.execute(
                r#"INSERT INTO api_routes (id,app_id,method,path,enabled,description)
                   VALUES ($1,$2,$3,$4,$5,$6)
                   ON CONFLICT(id) DO UPDATE SET app_id=excluded.app_id, method=excluded.method,
                     path=excluded.path, enabled=excluded.enabled, description=excluded.description"#,
                &[&record.id, &record.app_id, &record.method, &record.path,
                    &record.enabled, &record.description],
            ).await?;
        }
        SecurityMutation::DeleteApiRoute(id) => {
            transaction
                .execute("DELETE FROM api_routes WHERE id=$1", &[id])
                .await?;
        }
        SecurityMutation::UpsertTunnelRoute(record) => {
            transaction.execute(
                r#"INSERT INTO tunnel_routes (host,app_id,connector_endpoint,require_healthy_tunnel)
                   VALUES ($1,$2,$3,$4)
                   ON CONFLICT(host) DO UPDATE SET app_id=excluded.app_id,
                     connector_endpoint=excluded.connector_endpoint,
                     require_healthy_tunnel=excluded.require_healthy_tunnel"#,
                &[&record.host, &record.app_id, &record.connector_endpoint,
                    &record.require_healthy_tunnel],
            ).await?;
        }
        SecurityMutation::DeleteTunnelRoute(host) => {
            transaction
                .execute("DELETE FROM tunnel_routes WHERE host=$1", &[host])
                .await?;
        }
        SecurityMutation::UpsertIntranetUpstream(record) => {
            transaction
                .execute(
                    r#"INSERT INTO intranet_upstreams (app_id,upstream,scheme)
                   VALUES ($1,$2,$3)
                   ON CONFLICT(app_id) DO UPDATE SET upstream=excluded.upstream,
                     scheme=excluded.scheme"#,
                    &[&record.app_id, &record.upstream, &record.scheme],
                )
                .await?;
        }
    }
    Ok(())
}

fn auth_updated_at_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
