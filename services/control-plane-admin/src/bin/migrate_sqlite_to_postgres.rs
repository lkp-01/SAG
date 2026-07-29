use anyhow::{Context, Result};
use rusqlite::Connection;
use tokio_postgres::NoTls;

fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn sqlite_count(conn: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let n: i64 = conn.query_row(&sql, [], |r| r.get(0))?;
    Ok(n)
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

    let sqlite = Connection::open(&sqlite_path)
        .with_context(|| format!("open sqlite failed: {sqlite_path}"))?;
    let (mut pg, pg_conn) = tokio_postgres::connect(&pg_dsn, NoTls).await?;
    tokio::spawn(async move {
        let _ = pg_conn.await;
    });

    let pg_store =
        shared_storage::StorageStore::Postgres(shared_storage::PostgresStore::new(pg_dsn));
    shared_storage::ensure_store_schema(&pg_store).await?;
    shared_storage::PoliciesStore::init_schema(&pg_store).await?;

    let tx = pg.transaction().await?;

    // tunnel_routes
    {
        let mut stmt = sqlite.prepare(
            "SELECT host, app_id, connector_endpoint, require_healthy_tunnel FROM tunnel_routes",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        for row in rows {
            let (host, app_id, endpoint, healthy) = row?;
            tx.execute(
                r#"INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
                   VALUES ($1,$2,$3,$4)
                   ON CONFLICT(host) DO UPDATE SET
                     app_id=excluded.app_id,
                     connector_endpoint=excluded.connector_endpoint,
                     require_healthy_tunnel=excluded.require_healthy_tunnel"#,
                &[&host, &app_id, &endpoint, &healthy],
            )
            .await?;
        }
    }

    // intranet_upstreams
    {
        let mut stmt = sqlite.prepare("SELECT app_id, upstream, scheme FROM intranet_upstreams")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (app_id, upstream, scheme) = row?;
            tx.execute(
                r#"INSERT INTO intranet_upstreams (app_id, upstream, scheme)
                   VALUES ($1,$2,$3)
                   ON CONFLICT(app_id) DO UPDATE SET
                     upstream=excluded.upstream,
                     scheme=excluded.scheme"#,
                &[&app_id, &upstream, &scheme],
            )
            .await?;
        }
    }

    // policies
    {
        let mut stmt = sqlite.prepare(
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
        let mut stmt = sqlite.prepare(
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
            sqlite.prepare("SELECT app_id, display_name, description, enabled FROM apps")?;
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
        let mut stmt = sqlite
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
        let mut stmt = sqlite.prepare(
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
        let mut stmt = sqlite.prepare(
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
        let mut stmt = sqlite.prepare(
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
        let mut stmt = sqlite.prepare(
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
        let mut stmt = sqlite.prepare(
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

    tx.commit().await?;

    println!("SQLite -> PostgreSQL migration completed.");
    println!("source: {}", sqlite_path);
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
