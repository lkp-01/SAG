use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use tokio_postgres::NoTls;

const CURRENT_APISIX_APP_IDS_SQL: &str = "SELECT DISTINCT routes.app_id \
     FROM tunnel_routes AS routes \
     INNER JOIN intranet_upstreams AS upstreams ON upstreams.app_id = routes.app_id \
     WHERE length(trim(routes.app_id)) > 0 \
     ORDER BY routes.app_id";
const DESTINATION_HAS_DATA_SQL: &str = "SELECT EXISTS (\
    SELECT 1 FROM tunnel_routes UNION ALL \
    SELECT 1 FROM intranet_upstreams UNION ALL \
    SELECT 1 FROM policies UNION ALL \
    SELECT 1 FROM users UNION ALL \
    SELECT 1 FROM apps UNION ALL \
    SELECT 1 FROM api_routes UNION ALL \
    SELECT 1 FROM identity_providers UNION ALL \
    SELECT 1 FROM group_role_mappings UNION ALL \
    SELECT 1 FROM app_metrics_minute UNION ALL \
    SELECT 1 FROM audit_logs UNION ALL \
    SELECT 1 FROM fault_events UNION ALL \
    SELECT 1 FROM agent_config_applies UNION ALL \
    SELECT 1 FROM config_sync_jobs \
    LIMIT 1\
)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigConvergenceMigration {
    source_generation: i64,
    destination_generation_before: i64,
    generation: i64,
    route_jobs: usize,
}

#[derive(Debug)]
struct SourceRouteConfiguration {
    routes: Vec<shared_storage::TunnelRouteRecord>,
    upstreams: Vec<shared_storage::IntranetUpstreamRecord>,
}

fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn bool_env(key: &str) -> bool {
    std::env::var(key)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn sqlite_count(conn: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let n: i64 = conn.query_row(&sql, [], |r| r.get(0))?;
    Ok(n)
}

fn sqlite_config_generation(conn: &Connection) -> Result<i64> {
    let table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'config_state')",
        [],
        |row| row.get(0),
    )?;
    if !table_exists {
        return Ok(0);
    }
    let generation = conn
        .query_row(
            "SELECT generation FROM config_state WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    ensure!(
        generation >= 0,
        "source config_state.generation must be non-negative"
    );
    Ok(generation)
}

fn load_and_validate_source_route_configuration(
    connection: &Connection,
) -> Result<SourceRouteConfiguration> {
    let routes = {
        let mut statement = connection.prepare(
            "SELECT host, app_id, connector_endpoint, require_healthy_tunnel FROM tunnel_routes",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(shared_storage::TunnelRouteRecord {
                    host: row.get(0)?,
                    app_id: row.get(1)?,
                    connector_endpoint: row.get(2)?,
                    require_healthy_tunnel: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let upstreams = {
        let mut statement =
            connection.prepare("SELECT app_id, upstream, scheme FROM intranet_upstreams")?;
        let rows = statement
            .query_map([], |row| {
                Ok(shared_storage::IntranetUpstreamRecord {
                    app_id: row.get(0)?,
                    upstream: row.get(1)?,
                    scheme: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    shared_storage::validate_route_configuration_snapshot(&routes, &upstreams)
        .context("SQLite source route configuration failed convergence preflight")?;
    Ok(SourceRouteConfiguration { routes, upstreams })
}

async fn validate_postgres_route_configuration(
    transaction: &tokio_postgres::Transaction<'_>,
) -> Result<()> {
    let routes = transaction
        .query(
            "SELECT host, app_id, connector_endpoint, require_healthy_tunnel FROM tunnel_routes",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| shared_storage::TunnelRouteRecord {
            host: row.get(0),
            app_id: row.get(1),
            connector_endpoint: row.get(2),
            require_healthy_tunnel: row.get(3),
        })
        .collect::<Vec<_>>();
    let upstreams = transaction
        .query(
            "SELECT app_id, upstream, scheme FROM intranet_upstreams",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| shared_storage::IntranetUpstreamRecord {
            app_id: row.get(0),
            upstream: row.get(1),
            scheme: row.get(2),
        })
        .collect::<Vec<_>>();
    shared_storage::validate_route_configuration_snapshot(&routes, &upstreams)
        .context("merged PostgreSQL route configuration failed convergence preflight")?;
    Ok(())
}

fn next_migration_generation(
    source_generation: i64,
    destination_generation: i64,
    destination_job_generation: Option<i64>,
    destination_applied_generation: Option<i64>,
) -> Result<i64> {
    ensure!(
        source_generation >= 0
            && destination_generation >= 0
            && destination_job_generation.is_none_or(|generation| generation >= 0)
            && destination_applied_generation.is_none_or(|generation| generation >= 0),
        "configuration generations must be non-negative"
    );
    [
        Some(source_generation),
        Some(destination_generation),
        destination_job_generation,
        destination_applied_generation,
    ]
    .into_iter()
    .flatten()
    .max()
    .expect("source and destination generations are always present")
    .checked_add(1)
    .context("configuration generation exhausted i64")
}

fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

async fn migrate_config_convergence(
    transaction: &tokio_postgres::Transaction<'_>,
    source_generation: i64,
    migrated_at_ms: i64,
) -> Result<ConfigConvergenceMigration> {
    ensure!(
        migrated_at_ms >= 0,
        "migration timestamp must be non-negative"
    );

    let destination_generation_before = transaction
        .query_one(
            "SELECT generation FROM config_state WHERE id = 1 FOR UPDATE",
            &[],
        )
        .await?
        .get::<_, i64>(0);
    let destination_job_generation = transaction
        .query_one("SELECT MAX(generation) FROM config_sync_jobs", &[])
        .await?
        .get::<_, Option<i64>>(0);
    let destination_applied_generation = transaction
        .query_one(
            "SELECT MAX(applied_generation) FROM agent_config_applies",
            &[],
        )
        .await?
        .get::<_, Option<i64>>(0);
    let generation = next_migration_generation(
        source_generation,
        destination_generation_before,
        destination_job_generation,
        destination_applied_generation,
    )?;

    transaction
        .execute(
            "UPDATE config_state \
             SET generation = $1, updated_at_ms = GREATEST(updated_at_ms, $2) \
             WHERE id = 1",
            &[&generation, &migrated_at_ms],
        )
        .await?;

    // Source jobs are deliberately not copied: their leases, attempts, and
    // terminal state describe the old process/database. Likewise, source
    // Agent ACKs are not copied so a post-cutover ACK proves that an Agent read
    // the PostgreSQL snapshot. Retain destination history, but fence every old
    // APISIX generation (including APPLIED history) before inserting the sole
    // current row required by the partial unique invariant.
    transaction
        .execute(
            "UPDATE config_sync_jobs \
             SET superseded_by_generation = $1, updated_at_ms = $2 \
             WHERE target = 'APISIX' \
               AND generation < $1 \
               AND superseded_by_generation IS NULL",
            &[&generation, &migrated_at_ms],
        )
        .await?;

    // The APISIX desired-state contract defines an active app as the
    // intersection of tunnel routes and intranet upstreams. Seed a fresh full
    // UPSERT set so the durable worker can converge every desired route even
    // though source runtime jobs are intentionally discarded.
    let current_apps = transaction.query(CURRENT_APISIX_APP_IDS_SQL, &[]).await?;
    for row in &current_apps {
        let app_id = row.get::<_, String>(0);
        let job_id = uuid::Uuid::new_v4().to_string();
        transaction
            .execute(
                "INSERT INTO config_sync_jobs ( \
                   job_id, generation, target, resource_type, resource_id, app_id, operation, \
                   payload_json, status, attempt_count, next_attempt_at_ms, last_error, \
                   lease_owner, lease_expires_at_ms, superseded_by_generation, created_at_ms, \
                   updated_at_ms, applied_at_ms \
                 ) VALUES ( \
                   $1, $2, 'APISIX', 'ROUTE', $3, $3, 'UPSERT', \
                   NULL, 'PENDING', 0, $4, NULL, NULL, NULL, NULL, $4, $4, NULL \
                 )",
                &[&job_id, &generation, &app_id, &migrated_at_ms],
            )
            .await?;
    }

    Ok(ConfigConvergenceMigration {
        source_generation,
        destination_generation_before,
        generation,
        route_jobs: current_apps.len(),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let sqlite_path = env_or_default(
        "SAG_SRC_SQLITE_DB_PATH",
        shared_storage::DEFAULT_STORAGE_DB_REL_PATH,
    );
    let pg_dsn = std::env::var("SAG_DST_POSTGRES_DSN")
        .or_else(|_| std::env::var("SAG_POSTGRES_DSN"))
        .context("SAG_DST_POSTGRES_DSN or SAG_POSTGRES_DSN is required")?;

    let mut sqlite = Connection::open(&sqlite_path)
        .with_context(|| format!("open sqlite failed: {sqlite_path}"))?;
    let source = sqlite
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .context("start consistent SQLite source snapshot")?;
    let source_generation = sqlite_config_generation(&source)?;
    // Reject a poisoned legacy snapshot before opening the destination write
    // transaction. The same snapshot is reused for inserts so validation and
    // copy cannot observe different source rows.
    let source_route_configuration = load_and_validate_source_route_configuration(&source)?;
    let (mut pg, pg_conn) = tokio_postgres::connect(&pg_dsn, NoTls).await?;
    tokio::spawn(async move {
        let _ = pg_conn.await;
    });

    let pg_store =
        shared_storage::StorageStore::Postgres(shared_storage::PostgresStore::new(pg_dsn));
    shared_storage::ensure_store_schema(&pg_store).await?;
    shared_storage::PoliciesStore::init_schema(&pg_store).await?;

    let tx = pg.transaction().await?;
    let destination_has_data: bool = tx.query_one(DESTINATION_HAS_DATA_SQL, &[]).await?.get(0);
    ensure!(
        !destination_has_data || bool_env("SAG_MIGRATION_ALLOW_NONEMPTY_DESTINATION"),
        "PostgreSQL destination is not empty; migration is merge-only and will not delete destination-only rows. Use an empty destination, or set SAG_MIGRATION_ALLOW_NONEMPTY_DESTINATION=true only after reviewing merge semantics"
    );

    // tunnel_routes (already validated from the source snapshot)
    for record in &source_route_configuration.routes {
        tx.execute(
            r#"INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
                   VALUES ($1,$2,$3,$4)
                   ON CONFLICT(host) DO UPDATE SET
                     app_id=excluded.app_id,
                     connector_endpoint=excluded.connector_endpoint,
                     require_healthy_tunnel=excluded.require_healthy_tunnel"#,
            &[
                &record.host,
                &record.app_id,
                &record.connector_endpoint,
                &record.require_healthy_tunnel,
            ],
        )
        .await?;
    }

    // intranet_upstreams (already validated from the source snapshot)
    for record in &source_route_configuration.upstreams {
        tx.execute(
            r#"INSERT INTO intranet_upstreams (app_id, upstream, scheme)
                   VALUES ($1,$2,$3)
                   ON CONFLICT(app_id) DO UPDATE SET
                     upstream=excluded.upstream,
                     scheme=excluded.scheme"#,
            &[&record.app_id, &record.upstream, &record.scheme],
        )
        .await?;
    }
    // Merge mode can combine individually valid source and destination rows
    // into one invalid app. Validate the transaction's final snapshot before
    // generation/outbox seeding; any failure rolls back every copied row.
    validate_postgres_route_configuration(&tx).await?;

    // policies
    {
        let mut stmt = source.prepare(
            "SELECT id, effect, subjects_json, app_id, path_prefix, priority FROM policies",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        for row in rows {
            let (id, effect, subjects_json, app_id, path_prefix, priority) = row?;
            let priority_i32 = i32::try_from(priority).unwrap_or(1000);
            tx.execute(
                r#"INSERT INTO policies (id, effect, subjects_json, app_id, path_prefix, priority)
                   VALUES ($1,$2,$3,$4,$5,$6)
                   ON CONFLICT(id) DO UPDATE SET
                     effect=excluded.effect,
                     subjects_json=excluded.subjects_json,
                     app_id=excluded.app_id,
                     path_prefix=excluded.path_prefix,
                     priority=excluded.priority"#,
                &[
                    &id,
                    &effect,
                    &subjects_json,
                    &app_id,
                    &path_prefix,
                    &priority_i32,
                ],
            )
            .await?;
        }
    }

    // users
    {
        let mut stmt = source.prepare(
            "SELECT id, username, password_hash, roles_json, display_name, title, enabled FROM users",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)? != 0,
            ))
        })?;
        for row in rows {
            let (id, username, password_hash, roles_json, display_name, title, enabled) = row?;
            tx.execute(
                r#"INSERT INTO users (id, username, password_hash, roles_json, display_name, title, enabled)
                   VALUES ($1,$2,$3,$4,$5,$6,$7)
                   ON CONFLICT(username) DO UPDATE SET
                     id=excluded.id,
                     password_hash=excluded.password_hash,
                     roles_json=excluded.roles_json,
                     display_name=excluded.display_name,
                     title=excluded.title,
                     enabled=excluded.enabled"#,
                &[&id, &username, &password_hash, &roles_json, &display_name, &title, &enabled],
            )
            .await?;
        }
    }

    // apps
    {
        let mut stmt =
            source.prepare("SELECT app_id, display_name, description, enabled FROM apps")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        for row in rows {
            let (app_id, display_name, description, enabled) = row?;
            tx.execute(
                r#"INSERT INTO apps (app_id, display_name, description, enabled)
                   VALUES ($1,$2,$3,$4)
                   ON CONFLICT(app_id) DO UPDATE SET
                     display_name=excluded.display_name,
                     description=excluded.description,
                     enabled=excluded.enabled"#,
                &[&app_id, &display_name, &description, &enabled],
            )
            .await?;
        }
    }

    // api_routes
    {
        let mut stmt = source
            .prepare("SELECT id, app_id, method, path, enabled, description FROM api_routes")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)? != 0,
                r.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (id, app_id, method, path, enabled, description) = row?;
            tx.execute(
                r#"INSERT INTO api_routes (id, app_id, method, path, enabled, description)
                   VALUES ($1,$2,$3,$4,$5,$6)
                   ON CONFLICT(id) DO UPDATE SET
                     app_id=excluded.app_id,
                     method=excluded.method,
                     path=excluded.path,
                     enabled=excluded.enabled,
                     description=excluded.description"#,
                &[&id, &app_id, &method, &path, &enabled, &description],
            )
            .await?;
        }
    }

    // identity_providers
    {
        let mut stmt = source.prepare(
            "SELECT id, kind, issuer, client_id, client_secret, scopes, enabled FROM identity_providers",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)? != 0,
            ))
        })?;
        for row in rows {
            let (id, kind, issuer, client_id, client_secret, scopes, enabled) = row?;
            tx.execute(
                r#"INSERT INTO identity_providers (id, kind, issuer, client_id, client_secret, scopes, enabled)
                   VALUES ($1,$2,$3,$4,$5,$6,$7)
                   ON CONFLICT(id) DO UPDATE SET
                     kind=excluded.kind,
                     issuer=excluded.issuer,
                     client_id=excluded.client_id,
                     client_secret=excluded.client_secret,
                     scopes=excluded.scopes,
                     enabled=excluded.enabled"#,
                &[&id, &kind, &issuer, &client_id, &client_secret, &scopes, &enabled],
            )
            .await?;
        }
    }

    // group_role_mappings
    {
        let mut stmt = source.prepare(
            "SELECT id, provider_id, external_group, local_roles_csv, enabled, priority FROM group_role_mappings",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)? != 0,
                r.get::<_, i64>(5)?,
            ))
        })?;
        for row in rows {
            let (id, provider_id, external_group, local_roles_csv, enabled, priority) = row?;
            tx.execute(
                r#"INSERT INTO group_role_mappings (id, provider_id, external_group, local_roles_csv, enabled, priority)
                   VALUES ($1,$2,$3,$4,$5,$6)
                   ON CONFLICT(id) DO UPDATE SET
                     provider_id=excluded.provider_id,
                     external_group=excluded.external_group,
                     local_roles_csv=excluded.local_roles_csv,
                     enabled=excluded.enabled,
                     priority=excluded.priority"#,
                &[&id, &provider_id, &external_group, &local_roles_csv, &enabled, &priority],
            )
            .await?;
        }
    }

    // app_metrics_minute
    {
        let mut stmt = source.prepare(
            "SELECT ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg FROM app_metrics_minute",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, f64>(8)?,
            ))
        })?;
        for row in rows {
            let (
                ts_minute,
                app_id,
                request_count,
                pv_count,
                uv_count,
                unique_ip_count,
                err4xx_count,
                err5xx_count,
                qps_avg,
            ) = row?;
            tx.execute(
                r#"INSERT INTO app_metrics_minute
                   (ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg)
                   VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                   ON CONFLICT(ts_minute, app_id) DO UPDATE SET
                     request_count=excluded.request_count,
                     pv_count=excluded.pv_count,
                     uv_count=excluded.uv_count,
                     unique_ip_count=excluded.unique_ip_count,
                     err4xx_count=excluded.err4xx_count,
                     err5xx_count=excluded.err5xx_count,
                     qps_avg=excluded.qps_avg"#,
                &[&ts_minute, &app_id, &request_count, &pv_count, &uv_count, &unique_ip_count, &err4xx_count, &err5xx_count, &qps_avg],
            )
            .await?;
        }
    }

    // audit_logs
    {
        let mut stmt = source.prepare(
            "SELECT id, ts_ms, service, user_id, app_id, path, method, latency_ms, decision, result, trace_id, extra_json FROM audit_logs",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
            ))
        })?;
        for row in rows {
            let (
                id,
                ts_ms,
                service,
                user_id,
                app_id,
                path,
                method,
                latency_ms,
                decision,
                result,
                trace_id,
                extra_json,
            ) = row?;
            tx.execute(
                r#"INSERT INTO audit_logs
                   (id, ts_ms, service, user_id, app_id, path, method, latency_ms, decision, result, trace_id, extra_json)
                   VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                   ON CONFLICT(id) DO UPDATE SET
                     ts_ms=excluded.ts_ms,
                     service=excluded.service,
                     user_id=excluded.user_id,
                     app_id=excluded.app_id,
                     path=excluded.path,
                     method=excluded.method,
                     latency_ms=excluded.latency_ms,
                     decision=excluded.decision,
                     result=excluded.result,
                     trace_id=excluded.trace_id,
                     extra_json=excluded.extra_json"#,
                &[&id, &ts_ms, &service, &user_id, &app_id, &path, &method, &latency_ms, &decision, &result, &trace_id, &extra_json],
            )
            .await?;
        }
    }

    // fault_events
    {
        let mut stmt = source.prepare(
            "SELECT id, ts_ms, service, event_type, severity, path, method, latency_ms, baseline_ms, threshold_ms, status_code, result, trace_id, source, resolved_at_ms, meta_json FROM fault_events",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, Option<i64>>(14)?,
                r.get::<_, String>(15)?,
            ))
        })?;
        for row in rows {
            let (
                id,
                ts_ms,
                service,
                event_type,
                severity,
                path,
                method,
                latency_ms,
                baseline_ms,
                threshold_ms,
                status_code,
                result,
                trace_id,
                source,
                resolved_at_ms,
                meta_json,
            ) = row?;
            tx.execute(
                r#"INSERT INTO fault_events
                   (id, ts_ms, service, event_type, severity, path, method, latency_ms, baseline_ms, threshold_ms, status_code, result, trace_id, source, resolved_at_ms, meta_json)
                   VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
                   ON CONFLICT(id) DO UPDATE SET
                     ts_ms=excluded.ts_ms,
                     service=excluded.service,
                     event_type=excluded.event_type,
                     severity=excluded.severity,
                     path=excluded.path,
                     method=excluded.method,
                     latency_ms=excluded.latency_ms,
                     baseline_ms=excluded.baseline_ms,
                     threshold_ms=excluded.threshold_ms,
                     status_code=excluded.status_code,
                     result=excluded.result,
                     trace_id=excluded.trace_id,
                     source=excluded.source,
                     resolved_at_ms=excluded.resolved_at_ms,
                     meta_json=excluded.meta_json"#,
                &[&id, &ts_ms, &service, &event_type, &severity, &path, &method, &latency_ms, &baseline_ms, &threshold_ms, &status_code, &result, &trace_id, &source, &resolved_at_ms, &meta_json],
            )
            .await?;
        }
    }

    let convergence = migrate_config_convergence(&tx, source_generation, now_epoch_ms()).await?;

    // The source transaction is read-only. Finishing it before the PostgreSQL
    // commit keeps every source row and source generation from one snapshot;
    // operators must still quiesce writers because two databases cannot share
    // one atomic commit protocol.
    source
        .commit()
        .context("finish consistent SQLite source snapshot")?;
    tx.commit().await?;

    println!("SQLite -> PostgreSQL migration completed.");
    println!("source: {}", sqlite_path);
    println!(
        "config_generation[source={}, destination_before={}, destination_after={}]",
        convergence.source_generation,
        convergence.destination_generation_before,
        convergence.generation
    );
    println!("config_sync_jobs[APISIX/ROUTE]={}", convergence.route_jobs);
    println!(
        "rows[tunnel_routes]={}",
        sqlite_count(&sqlite, "tunnel_routes").unwrap_or(0)
    );
    println!(
        "rows[intranet_upstreams]={}",
        sqlite_count(&sqlite, "intranet_upstreams").unwrap_or(0)
    );
    println!(
        "rows[policies]={}",
        sqlite_count(&sqlite, "policies").unwrap_or(0)
    );
    println!(
        "rows[users]={}",
        sqlite_count(&sqlite, "users").unwrap_or(0)
    );
    println!("rows[apps]={}", sqlite_count(&sqlite, "apps").unwrap_or(0));
    println!(
        "rows[api_routes]={}",
        sqlite_count(&sqlite, "api_routes").unwrap_or(0)
    );
    println!(
        "rows[identity_providers]={}",
        sqlite_count(&sqlite, "identity_providers").unwrap_or(0)
    );
    println!(
        "rows[group_role_mappings]={}",
        sqlite_count(&sqlite, "group_role_mappings").unwrap_or(0)
    );
    println!(
        "rows[app_metrics_minute]={}",
        sqlite_count(&sqlite, "app_metrics_minute").unwrap_or(0)
    );
    println!(
        "rows[audit_logs]={}",
        sqlite_count(&sqlite, "audit_logs").unwrap_or(0)
    );
    println!(
        "rows[fault_events]={}",
        sqlite_count(&sqlite, "fault_events").unwrap_or(0)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_generation_advances_past_all_durable_convergence_state() {
        assert_eq!(
            next_migration_generation(7, 3, Some(6), Some(5)).unwrap(),
            8
        );
        assert_eq!(
            next_migration_generation(2, 9, Some(4), Some(8)).unwrap(),
            10
        );
        assert_eq!(
            next_migration_generation(2, 3, Some(11), Some(8)).unwrap(),
            12
        );
        assert_eq!(
            next_migration_generation(2, 3, Some(4), Some(13)).unwrap(),
            14
        );
    }

    #[test]
    fn migration_generation_rejects_invalid_or_exhausted_state() {
        assert!(next_migration_generation(-1, 0, None, None).is_err());
        assert!(next_migration_generation(0, 0, Some(-1), None).is_err());
        assert!(next_migration_generation(i64::MAX, 0, None, None).is_err());
    }

    #[test]
    fn legacy_source_without_config_state_starts_at_generation_zero() {
        let connection = Connection::open_in_memory().unwrap();
        assert_eq!(sqlite_config_generation(&connection).unwrap(), 0);

        connection
            .execute_batch(
                "CREATE TABLE config_state (id INTEGER PRIMARY KEY, generation INTEGER NOT NULL);",
            )
            .unwrap();
        assert_eq!(sqlite_config_generation(&connection).unwrap(), 0);

        connection
            .execute(
                "INSERT INTO config_state (id, generation) VALUES (1, 42)",
                [],
            )
            .unwrap();
        assert_eq!(sqlite_config_generation(&connection).unwrap(), 42);
    }

    #[test]
    fn full_route_seed_contains_only_distinct_current_apisix_apps() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE tunnel_routes (host TEXT PRIMARY KEY, app_id TEXT NOT NULL); \
                 CREATE TABLE intranet_upstreams (app_id TEXT PRIMARY KEY); \
                 INSERT INTO tunnel_routes (host, app_id) VALUES \
                   ('a-1.internal', 'app-a'), \
                   ('a-2.internal', 'app-a'), \
                   ('b.internal', 'app-b'), \
                   ('route-only.internal', 'route-only'), \
                   ('blank.internal', '   '); \
                 INSERT INTO intranet_upstreams (app_id) VALUES \
                   ('app-a'), ('app-b'), ('upstream-only'), ('   ');",
            )
            .unwrap();

        let mut statement = connection.prepare(CURRENT_APISIX_APP_IDS_SQL).unwrap();
        let apps = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();

        assert_eq!(apps, vec!["app-a", "app-b"]);
    }

    #[test]
    fn source_preflight_rejects_conflicting_routes_and_invalid_upstreams() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE tunnel_routes ( \
                   host TEXT PRIMARY KEY, app_id TEXT NOT NULL, \
                   connector_endpoint TEXT NOT NULL, require_healthy_tunnel INTEGER NOT NULL \
                 ); \
                 CREATE TABLE intranet_upstreams ( \
                   app_id TEXT PRIMARY KEY, upstream TEXT NOT NULL, scheme TEXT NOT NULL \
                 ); \
                 INSERT INTO tunnel_routes VALUES \
                   ('a.internal', 'app-a', 'connector-a:stream', 1), \
                   ('b.internal', 'app-a', 'connector-b:stream', 1); \
                 INSERT INTO intranet_upstreams VALUES \
                   ('app-a', 'upstream.internal:8080', 'http');",
            )
            .unwrap();

        let conflict = load_and_validate_source_route_configuration(&connection).unwrap_err();
        assert!(conflict
            .chain()
            .any(|cause| cause.to_string().contains("same connector_endpoint")));

        connection
            .execute(
                "UPDATE tunnel_routes SET connector_endpoint = 'connector-a:stream' \
                 WHERE host = 'b.internal'",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE intranet_upstreams SET upstream = 'bad!host:8080' WHERE app_id = 'app-a'",
                [],
            )
            .unwrap();
        let invalid = load_and_validate_source_route_configuration(&connection).unwrap_err();
        assert!(invalid
            .chain()
            .any(|cause| cause.to_string().contains("valid IPv4 address or DNS name")));

        connection
            .execute(
                "UPDATE intranet_upstreams SET upstream = '[2001:db8::1]:8443', scheme = 'https' \
                 WHERE app_id = 'app-a'",
                [],
            )
            .unwrap();
        let validated = load_and_validate_source_route_configuration(&connection).unwrap();
        assert_eq!(validated.routes.len(), 2);
        assert_eq!(validated.upstreams.len(), 1);
    }
}
