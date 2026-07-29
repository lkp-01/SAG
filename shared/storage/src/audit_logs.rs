use serde::{Deserialize, Serialize};

use crate::{
    ApiRouteRecord, AppRecord, GroupRoleMappingRecord, IdentityProviderRecord,
    IntranetUpstreamRecord, PolicyEffect, PolicyRecord, StorageError, StorageStore,
    TunnelRouteRecord, UserRecord,
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
                    let transaction = connection.transaction()?;
                    execute_sqlite_security_mutation(&transaction, &mutation)?;
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
                execute_postgres_security_mutation(&transaction, mutation).await?;
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
