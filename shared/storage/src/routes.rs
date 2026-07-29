use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelRouteRecord {
    pub host: String,
    pub app_id: String,
    pub connector_endpoint: String,
    pub require_healthy_tunnel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntranetUpstreamRecord {
    pub app_id: String,
    pub upstream: String,
    pub scheme: String,
}

pub struct RoutesStore;

impl RoutesStore {
    pub async fn upsert(
        store: &StorageStore,
        record: &TunnelRouteRecord,
    ) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    conn.execute(
                        r"INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
                          VALUES (?1, ?2, ?3, ?4)
                          ON CONFLICT(host) DO UPDATE SET
                            app_id=excluded.app_id,
                            connector_endpoint=excluded.connector_endpoint,
                            require_healthy_tunnel=excluded.require_healthy_tunnel",
                        rusqlite::params![
                            r.host,
                            r.app_id,
                            r.connector_endpoint,
                            if r.require_healthy_tunnel { 1i32 } else { 0i32 }
                        ],
                    )?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute(
                        r#"INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
                           VALUES ($1, $2, $3, $4)
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
                Ok(())
            }
        }
    }

    pub async fn delete_by_host(store: &StorageStore, host: &str) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let host = host.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    conn.execute("DELETE FROM tunnel_routes WHERE host = ?1", [&host])?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute("DELETE FROM tunnel_routes WHERE host = $1", &[&host])
                    .await?;
                Ok(())
            }
        }
    }

    /// Inserts the default local-dev tunnel row only if `tunnel_routes` is empty.
    pub async fn insert_demo_route_if_empty(store: &StorageStore) -> Result<bool, StorageError> {
        let demo = TunnelRouteRecord {
            host: "app.internal.com".into(),
            app_id: "app-001".into(),
            connector_endpoint: "connector-local-001:stream".into(),
            require_healthy_tunnel: true,
        };
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    let count: i64 = conn.query_row("SELECT COUNT(*) FROM tunnel_routes", [], |row| {
                        row.get(0)
                    })?;
                    if count > 0 {
                        return Ok(false);
                    }
                    conn.execute(
                        r"INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
                          VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![
                            demo.host,
                            demo.app_id,
                            demo.connector_endpoint,
                            if demo.require_healthy_tunnel { 1i32 } else { 0i32 }
                        ],
                    )?;
                    Ok(true)
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let row = client
                    .query_one("SELECT COUNT(*)::BIGINT FROM tunnel_routes", &[])
                    .await?;
                let count: i64 = row.get(0);
                if count > 0 {
                    return Ok(false);
                }
                client
                    .execute(
                        r#"INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
                           VALUES ($1, $2, $3, $4)"#,
                        &[
                            &demo.host,
                            &demo.app_id,
                            &demo.connector_endpoint,
                            &demo.require_healthy_tunnel,
                        ],
                    )
                    .await?;
                Ok(true)
            }
        }
    }

    pub async fn load_all(store: &StorageStore) -> Result<Vec<TunnelRouteRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    let mut stmt = conn.prepare(
                        "SELECT host, app_id, connector_endpoint, require_healthy_tunnel FROM tunnel_routes",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        Ok(TunnelRouteRecord {
                            host: row.get(0)?,
                            app_id: row.get(1)?,
                            connector_endpoint: row.get(2)?,
                            require_healthy_tunnel: row.get::<_, i32>(3)? != 0,
                        })
                    })?;
                    let mut out = Vec::new();
                    for r in rows {
                        out.push(r?);
                    }
                    Ok::<_, StorageError>(out)
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let rows = client
                    .query(
                        "SELECT host, app_id, connector_endpoint, require_healthy_tunnel FROM tunnel_routes",
                        &[],
                    )
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|r| TunnelRouteRecord {
                        host: r.get(0),
                        app_id: r.get(1),
                        connector_endpoint: r.get(2),
                        require_healthy_tunnel: r.get(3),
                    })
                    .collect())
            }
        }
    }

    pub async fn upsert_intranet_upstream(
        store: &StorageStore,
        record: &IntranetUpstreamRecord,
    ) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    conn.execute(
                        r"INSERT INTO intranet_upstreams (app_id, upstream, scheme)
                          VALUES (?1, ?2, ?3)
                          ON CONFLICT(app_id) DO UPDATE SET upstream=excluded.upstream, scheme=excluded.scheme",
                        rusqlite::params![r.app_id, r.upstream, r.scheme],
                    )?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute(
                        r#"INSERT INTO intranet_upstreams (app_id, upstream, scheme)
                           VALUES ($1, $2, $3)
                           ON CONFLICT(app_id) DO UPDATE SET upstream=excluded.upstream, scheme=excluded.scheme"#,
                        &[&record.app_id, &record.upstream, &record.scheme],
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn get_intranet_upstream(
        store: &StorageStore,
        app_id: &str,
    ) -> Result<Option<IntranetUpstreamRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let app_id = app_id.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    let mut stmt = conn.prepare(
                        "SELECT app_id, upstream, scheme FROM intranet_upstreams WHERE app_id = ?1",
                    )?;
                    let mut rows = stmt.query_map([&app_id], |row| {
                        Ok(IntranetUpstreamRecord {
                            app_id: row.get(0)?,
                            upstream: row.get(1)?,
                            scheme: row.get(2)?,
                        })
                    })?;
                    Ok(rows.next().transpose()?)
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let row = client
                    .query_opt(
                        "SELECT app_id, upstream, scheme FROM intranet_upstreams WHERE app_id = $1",
                        &[&app_id],
                    )
                    .await?;
                Ok(row.map(|r| IntranetUpstreamRecord {
                    app_id: r.get(0),
                    upstream: r.get(1),
                    scheme: r.get(2),
                }))
            }
        }
    }
}
